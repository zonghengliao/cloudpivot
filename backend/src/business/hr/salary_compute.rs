//! 薪资**计算引擎**（试算 → 逐人逐项 → 恒等式校验 → 落库事务 · HR-34）。
//!
//! 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
//! §4.4（五类薪资项 / 公式 DSL / 17 个种子项 / 实测算例 689.66·735.63·9954.03·8154.03）、
//! §4.8.4（期中入职离职调薪，一期整月计入）、§4.8.5（负数与补发）、
//! §4.9 的 A6/A7（薪资单三方对账）、§5.2 #39/#40（试算 / 建单落库）。
//!
//! # 一条数据链
//!
//! ```text
//! 薪资项 biz_salary_item ──HR-33 的 salary_vars::build_plan──▶ ComputePlan（拓扑序 + 变量解析）
//! 员工 biz_employee ────────────┐
//! 薪资标准 biz_employee_salary ─┤──▶ PayInput（每人一份取数快照）
//! 考勤台账 biz_attendance_record ┤
//! 人工录入（试算入参）──────────┘
//!                                │
//!                                ▼  逐人逐项求值（一个公式算 N 人）
//!                          Payslip（项 + 汇总 + 恒等式）
//!                                │
//!                                ▼  落库事务（单头 + 明细 + 明细项 + 状态日志）
//!          biz_salary_sheet / biz_salary_detail / biz_salary_detail_item
//! ```
//!
//! # 试算与正式计算**同源**（本卡验收第 3 条）
//!
//! 只有一条编排路径 [`calc_with_items`]：
//!
//! | 入参 | 行为 | 终点 |
//! |---|---|---|
//! | `dry_run = true` | 同一事务里读 → 算 → **rollback** | 返回逐人逐项结果，库里零行 |
//! | `dry_run = false` | 同一事务里读 → 算 → 写单头/明细/明细项 → **commit** | 返回单号 + 逐人逐项结果 |
//!
//! 「试算」不是一个单独的实现，而是同一条路径把事务回滚掉 —— 两套实现必然
//! 「试算 9954.03、落库 9954.04」地漂（设计 §4.4.2：前端不做求值，只调后端）。
//! 事务化读的第二个好处：算工资的过程中考勤被别的会话改了，试算与落库也不会各读一半。
//!
//! # 恒等式（两道闸：引擎断言 + 库层 `CHECK`）
//!
//! ```text
//! 逐人：应发 gross    = Σ EARNING − Σ PRE_DEDUCT
//!       代扣 withhold = 社保个人 + 公积金个人 + 个税 + 其他代扣
//!       实发 net      = 应发 − 代扣
//! 交叉：公式项 GROSS / NET 必须等于「按项类型聚合」的应发 / 实发
//! 单头：gross_total = Σ detail.gross_amount、net_total = Σ detail.net_amount（巡检 A6）
//! ```
//!
//! 引擎在落库**之前**逐人复算一遍（[`check_identity`] + [`Engine::check_summary`]）：库层
//! `CHECK` 是最后一道闸，但它给用户看的是「违反了 ck_biz_sal_det_net」这种运维语言；引擎给的是
//! 「员工 EMP-0001（张三）的应发合计不一致：按项类型聚合是 9954.03，公式项 GROSS 是 9954.04」。
//! 公式交叉校验还挡住一类库层 `CHECK` 看不出来的错：某项 `item_type` 配错（聚合侧加、公式侧减），
//! 两侧各自自洽但口径不同。
//!
//! # 三类取数，各有各的坑
//!
//! | 算法 | 取数 | 取不到怎么办 |
//! |---|---|---|
//! | `FIXED` | `biz_employee_salary` 当期生效标准（`effective_from <= 期末` 且 `effective_to IS NULL OR >= 期初`） | 记 0，`calc_note` 写「未配标准」；**同项两条区间重叠 → 直接报错**（否则取到哪条是 `ORDER BY` 的随机数，设计 §3.1 ③ 的原话） |
//! | `INPUT` | 试算入参的人工项（社保 / 公积金 / 个税 / 绩效 / 补发） | 记 0，`calc_note` 写「未录入」 |
//! | `FORMULA` | [`crate::business::hr::salary_formula`] 求值（变量与拓扑序由 [`salary_vars`] 解析） | 除零 / 未定义变量 → 报错并点名**人 + 项**（静默 0 会让错工资一路走到凭证） |
//!
//! # 人口口径（谁进这张工资表）
//!
//! `hire_date <= 期末 AND (leave_date IS NULL OR leave_date >= 期初)` 的未删员工 ——
//! 一期**整月计入**（设计 §4.8.4：入职当月按整月发、离职当月同理，差额由 HR 在明细上手工改）。
//! 没有当期考勤记录的人**照样进表**（考勤变量按 0 计、`remark` 写明），但会出现在
//! [`CalcOutcome::missing_attendance`] 里让 HR 一眼看见 —— 少发一个人比多发一个人难补救得多。
//!
//! # 单位承担项为什么可能报错
//!
//! `biz_salary_detail` 只有 `si_company` / `hf_company` 两列（设计 §3.1 ⑨，迁移已应用不可改）。
//! 一期种子的 `EMPLOYER` 项正好是 `SI_COMPANY` / `HF_COMPANY`；客户新增第三个单位承担项时，
//! 引擎**报错点名**而不是猜一列塞进去（塞错列 = 凭证金额对不上，而且没人看得出来）。
//! 正确处置是加迁移补列，或把新项归到这两个编码下 —— 本文件不替用户决定。
//!
//! # 错误出口
//!
//! 纯函数（[`Engine::compute`] / [`compute_all`] / [`check_identity`]）只回 [`CalcIssue`]
//! （自带中文报文），由 [`issues_error`] 一次性翻成 `400 + 10001`；读写库的函数回
//! `DbResult` / `ApiError`（`?` 直通）。金额一律 `Decimal`：中间 10 位（解析器）、
//! 公式结果 2 位（`ROUND_HALF_UP`）、库列 4 位（设计 §4.4.2「用 f64 会出 0.1+0.2=0.30000000000000004」）。

use std::collections::{BTreeMap, BTreeSet};

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_model::{self as period_model};
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::hr::salary_formula::{self, Expr};
use crate::business::hr::salary_item_dto::{
    CALC_TYPE_FIXED, CALC_TYPE_INPUT, ITEM_TYPE_EARNING, ITEM_TYPE_EMPLOYER, ITEM_TYPE_PRE_DEDUCT, ITEM_TYPE_SUMMARY,
    ITEM_TYPE_WITHHOLD,
};
use crate::business::hr::salary_item_model::{self as item_model, SalaryItemRow};
use crate::business::hr::salary_vars::{self, ComputePlan, ItemSpec, VarIssue};
use crate::business::shared::state_machine::{self, Operator};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 薪资单状态：草稿（刚建、还没算）。
pub const STATUS_DRAFT: &str = "DRAFT";

/// 薪资单状态：已试算（逐人逐项落库，可提交审批）。
pub const STATUS_CALCULATED: &str = "CALCULATED";

/// 金额列的小数位（`NUMERIC(18,4)` / `NUMERIC(20,4)`）。
pub const AMOUNT_SCALE: u32 = 4;

/// 快照列的小数位（`should_days NUMERIC(6,2)` / `ot_hours NUMERIC(8,2)`）。
pub const SNAPSHOT_SCALE: u32 = 2;

/// `calc_note` / `expr_snapshot` 的列宽（`VARCHAR(500)`）。
pub const NOTE_MAX_LEN: usize = 500;

/// 薪资单号号段（`biz_seq.seq_type`，`V0386` 种入：`SAL-yyyyMM-000001`）。
pub const SHEET_SEQ_TYPE: &str = "SAL";

/// 建单日志 / 状态流转用的实体类型（`DocKind::SalarySheet`）。
const SALARY_SHEET_ENTITY: &str = state_machine::ENTITY_SALARY_SHEET;

/// 代扣四个「明细列桶」对应的薪资项编码（设计 §3.1 ⑨ 的四列）。
pub const WITHHOLD_SOCIAL_CODE: &str = "SI_PERSONAL";
/// 公积金个人。
pub const WITHHOLD_FUND_CODE: &str = "HF_PERSONAL";
/// 个人所得税。
pub const WITHHOLD_IIT_CODE: &str = "IIT";

/// 单位承担两项对应的编码（`si_company` / `hf_company` 两列）。
pub const EMPLOYER_SOCIAL_CODE: &str = "SI_COMPANY";
/// 单位公积金。
pub const EMPLOYER_FUND_CODE: &str = "HF_COMPANY";

/// 系统汇总项编码（库层 `is_system = 1`，设计 §4.4.3 #160/#170）。
///
/// 只用来做**交叉校验**（公式值 vs 聚合值）；`SUMMARY` 项本身不落 `detail_item`
/// （它的值已经在 `detail.gross_amount` / `net_amount` 列上，设计 §4.4.3 末尾注）。
pub const SUMMARY_GROSS_CODE: &str = "GROSS";
/// 实发合计。
pub const SUMMARY_NET_CODE: &str = "NET";

/// 一次试算最多回多少条问题报文（其余折叠成「还有 N 条」）。
pub const MAX_REPORTED_ISSUES: usize = 5;

/// 明细批量插入的分块行数（27 列 × 60 行 = 1620 个占位符，远低于 PG 的 65535 上限）。
const DETAIL_CHUNK: usize = 60;

/// 明细项批量插入的分块行数（13 列 × 300 行 = 3900 个占位符）。
const ITEM_CHUNK: usize = 300;

/// 舍入策略：与 `salary_formula::ROUNDING` 同一取值（财务口径四舍五入）。
const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 错误 ====================

/// 某一步算不下去（**不是**库错误，也不是入参格式错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalcIssue {
    /// 一个启用中的薪资项都没有。
    NoActiveItems,
    /// 同一员工同一项的薪资标准在当期有两条生效区间重叠（设计 §3.1 ③ + 巡检 A3）。
    StandardOverlap {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
        /// 命中的生效起（升序）。
        effective_from: Vec<NaiveDate>,
    },
    /// 公式求值失败（除零 / 未定义变量 / 类型不符）。
    Formula {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
        /// 求值器报文。
        message: String,
    },
    /// 新的单位承担项没有对应的明细列（见模块文档）。
    UnclassifiedEmployer {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
        /// 薪资项名称。
        item_name: String,
    },
    /// 某个项的金额是负数，而它所在的类型不允许为负（`WITHHOLD` / `EMPLOYER`）。
    NegativeAmount {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
        /// 算出来的金额。
        amount: Decimal,
    },
    /// 应发为负（考勤扣款超过收入）。设计 §4.8.5：一期不允许负应发。
    NegativeGross {
        /// 工号。
        emp_no: String,
        /// 姓名。
        name: String,
        /// 应发。
        gross: Decimal,
        /// 收入合计。
        earning: Decimal,
        /// 应发前扣减合计。
        pre_deduct: Decimal,
    },
    /// 实发为负（代扣超过应发）。
    NegativeNet {
        /// 工号。
        emp_no: String,
        /// 姓名。
        name: String,
        /// 实发。
        net: Decimal,
        /// 应发。
        gross: Decimal,
        /// 代扣合计。
        withhold: Decimal,
    },
    /// 恒等式不成立：`expected` 是「按项类型聚合」的期望值，`actual` 是另一侧。
    Identity {
        /// 工号。
        emp_no: String,
        /// 姓名。
        name: String,
        /// 哪条恒等式（中文）。
        what: &'static str,
        /// 期望值。
        expected: Decimal,
        /// 实际值。
        actual: Decimal,
    },
    /// 人工录入指向了不存在的薪资项编码。
    UnknownManualItem {
        /// 编码。
        item_code: String,
    },
    /// 人工录入指向了不在本次试算范围内的人。
    UnknownManualEmployee {
        /// 员工 id（`Uuid` → 字符串）。
        employee_id: String,
    },
    /// 同一人同一项的人工录入出现了两次。
    DuplicateManualEntry {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
    },
    /// 人工录入的项不是 `INPUT`：`FIXED` 取标准、`FORMULA` 按公式算，都不接受手工值。
    ManualNotAllowed {
        /// 工号。
        emp_no: String,
        /// 薪资项编码。
        item_code: String,
        /// 该项的算法。
        calc_type: String,
    },
}

impl CalcIssue {
    /// 可直接展示给用户的中文报文。
    pub fn message(&self) -> String {
        match self {
            Self::NoActiveItems => "没有任何启用中的薪资项：试算要按薪资项逐项计算，\
                 请先在「薪资项配置」里建项或启用（设计 §4.4.3 的 17 个种子项由迁移种入）"
                .to_string(),
            Self::StandardOverlap {
                emp_no,
                item_code,
                effective_from,
            } => {
                let days: Vec<String> = effective_from.iter().map(ToString::to_string).collect();
                format!(
                    "员工 {emp_no} 的薪资项 `{item_code}` 在当期有 {} 条生效区间重叠的标准（生效起：{}）：\
                     取哪一条都是随机的，请先把区间收口（旧行写 `effective_to = 新生效日 - 1 天`）",
                    days.len(),
                    days.join(" / ")
                )
            }
            Self::Formula {
                emp_no,
                item_code,
                message,
            } => format!("员工 {emp_no} 的薪资项 `{item_code}` 公式算不出来：{message}"),
            Self::UnclassifiedEmployer {
                emp_no,
                item_code,
                item_name,
            } => format!(
                "员工 {emp_no} 的单位承担项 `{item_code}`（{item_name}）没有对应的明细列：\
                 `biz_salary_detail` 只有 si_company / hf_company 两列（设计 §3.1 ⑨）；\
                 新增单位承担项要先加迁移补列，或把它归到 `{EMPLOYER_SOCIAL_CODE}` / `{EMPLOYER_FUND_CODE}` 下"
            ),
            Self::NegativeAmount {
                emp_no,
                item_code,
                amount,
            } => format!(
                "员工 {emp_no} 的薪资项 `{item_code}` 算出了负数（{amount}）：代扣 / 单位承担项必须 ≥ 0，\
                 请检查该项的公式或人工录入值"
            ),
            Self::NegativeGross {
                emp_no,
                name,
                gross,
                earning,
                pre_deduct,
            } => format!(
                "员工 {emp_no}（{name}）应发为负（{gross}）：收入合计 {earning} 减去应发前扣减 {pre_deduct} 小于 0。\
                 设计 §4.8.5 一期不允许负应发 —— 请到明细覆盖里调小扣款项，或把多余扣款留到下月处理"
            ),
            Self::NegativeNet {
                emp_no,
                name,
                net,
                gross,
                withhold,
            } => format!(
                "员工 {emp_no}（{name}）实发为负（{net}）：应发 {gross} 减去代扣 {withhold} 小于 0。\
                 请核对社保 / 公积金 / 个税 / 其他代扣的录入值"
            ),
            Self::Identity {
                emp_no,
                name,
                what,
                expected,
                actual,
            } => format!(
                "员工 {emp_no}（{name}）的{what}不一致：按项类型聚合是 {expected}，另一侧是 {actual}。\
                 这说明薪资项的 `item_type` 或公式口径配错了（库层 CHECK 会拒绝落库）"
            ),
            Self::UnknownManualItem { item_code } => {
                format!("人工录入引用了不存在的薪资项 `{item_code}`：编码必须是「薪资项配置」里启用中的 `item_code`")
            }
            Self::UnknownManualEmployee { employee_id } => format!(
                "人工录入指向的员工 {employee_id} 不在本次试算范围内（不在当期在职员工里，或已删除）：\
                 请核对员工 id，或先确认该员工的入职 / 离职日期"
            ),
            Self::DuplicateManualEntry { emp_no, item_code } => {
                format!("员工 {emp_no} 的薪资项 `{item_code}` 人工录入了两次：同一项一次试算只能有一个值")
            }
            Self::ManualNotAllowed {
                emp_no,
                item_code,
                calc_type,
            } => format!(
                "员工 {emp_no} 的薪资项 `{item_code}` 是 `{calc_type}` 项，不接受人工录入值\
                 （`FIXED` 取薪资标准、`FORMULA` 按公式算）；要改已算出的金额请走明细覆盖（HR-37，会留审计）"
            ),
        }
    }
}

impl std::fmt::Display for CalcIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// 把问题清单翻成一条 `400 + 10001`（最多列 [`MAX_REPORTED_ISSUES`] 条，其余折叠）。
pub fn issues_error(issues: &[CalcIssue]) -> ApiError {
    match issues.len() {
        0 => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "薪资试算失败：没有给出原因（这是引擎 bug，请报给开发）",
        ),
        1 => ApiError::new(400, error_code::PARAM_INVALID, issues[0].message()),
        n => {
            let head: Vec<String> = issues
                .iter()
                .take(MAX_REPORTED_ISSUES)
                .map(CalcIssue::message)
                .collect();
            let rest = n.saturating_sub(MAX_REPORTED_ISSUES);
            let tail = if rest > 0 {
                format!("（另有 {rest} 条同类问题，修完这些再试算一次即可看到）")
            } else {
                String::new()
            };
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("薪资试算发现 {n} 处问题：{}。{tail}", head.join("；")),
            )
        }
    }
}

/// 语法层问题（HR-33 的 [`VarIssue`]）→ 400，报文一次列全（配置期一轮改完）。
pub fn plan_error(issues: &[VarIssue]) -> ApiError {
    let head: Vec<String> = issues.iter().take(MAX_REPORTED_ISSUES).map(VarIssue::message).collect();
    let rest = issues.len().saturating_sub(MAX_REPORTED_ISSUES);
    let tail = if rest > 0 {
        format!("（另有 {rest} 条，修完再试算）")
    } else {
        String::new()
    };
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!("薪资项的公式配置有问题，不能试算：{}。{tail}", head.join("；")),
    )
}

// ==================== 取数结构 ====================

/// 考勤台账的 11 个数值列（与 `biz_attendance_record` 逐列对应）。
///
/// 为什么整行都取：`salary_vars::AttBinding` 给的是**公式真用到**的列，而明细的 4 个快照列
/// 无论如何都要写；且「这一期多录了个迟到次数」不该让试算失败 —— 一次取全比按需拼 SQL 稳。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttendanceNumbers {
    /// 应出勤天数。
    pub should_days: Decimal,
    /// 实际出勤天数。
    pub actual_days: Decimal,
    /// 带薪假天数。
    pub leave_paid_days: Decimal,
    /// 无薪假天数（扣款依据）。
    pub leave_unpaid_days: Decimal,
    /// 病假天数。
    pub sick_days: Decimal,
    /// 旷工天数。
    pub absent_days: Decimal,
    /// 平时加班小时。
    pub ot_hours_workday: Decimal,
    /// 休息日加班小时。
    pub ot_hours_weekend: Decimal,
    /// 节假日加班小时。
    pub ot_hours_holiday: Decimal,
    /// 迟到次数。
    pub late_count: Decimal,
    /// 早退次数。
    pub early_leave_count: Decimal,
}

impl AttendanceNumbers {
    /// 按台账**列名**取值（`salary_vars::AttBinding.column`）。
    ///
    /// 「公式里的 `{ATT_*}` → 台账哪一列」的唯一落点：单测
    /// `every_whitelisted_column_is_readable` 拿 `salary_item_formula::ATT_VARS` 逐条对拍，
    /// 白名单加列而这里忘加会当场变红（否则公式里写 `{ATT_LATE_COUNT}` 会静默算成 0）。
    pub fn value_of(&self, column: &str) -> Option<Decimal> {
        Some(match column {
            "should_days" => self.should_days,
            "actual_days" => self.actual_days,
            "leave_paid_days" => self.leave_paid_days,
            "leave_unpaid_days" => self.leave_unpaid_days,
            "sick_days" => self.sick_days,
            "absent_days" => self.absent_days,
            "ot_hours_workday" => self.ot_hours_workday,
            "ot_hours_weekend" => self.ot_hours_weekend,
            "ot_hours_holiday" => self.ot_hours_holiday,
            "late_count" => self.late_count,
            "early_leave_count" => self.early_leave_count,
            _ => return None,
        })
    }

    /// 加班小时合计（明细的 `ot_hours` 快照列）。
    pub fn ot_hours_total(&self) -> Decimal {
        self.ot_hours_workday + self.ot_hours_weekend + self.ot_hours_holiday
    }

    /// 收口到明细表的两位列（`NUMERIC(6,2)` / `NUMERIC(8,2)`）。
    pub fn round_snapshot(&self) -> Self {
        Self {
            should_days: round_to(self.should_days, SNAPSHOT_SCALE),
            actual_days: round_to(self.actual_days, SNAPSHOT_SCALE),
            leave_paid_days: round_to(self.leave_paid_days, SNAPSHOT_SCALE),
            leave_unpaid_days: round_to(self.leave_unpaid_days, SNAPSHOT_SCALE),
            sick_days: round_to(self.sick_days, SNAPSHOT_SCALE),
            absent_days: round_to(self.absent_days, SNAPSHOT_SCALE),
            ot_hours_workday: round_to(self.ot_hours_workday, SNAPSHOT_SCALE),
            ot_hours_weekend: round_to(self.ot_hours_weekend, SNAPSHOT_SCALE),
            ot_hours_holiday: round_to(self.ot_hours_holiday, SNAPSHOT_SCALE),
            late_count: round_to(self.late_count, SNAPSHOT_SCALE),
            early_leave_count: round_to(self.early_leave_count, SNAPSHOT_SCALE),
        }
    }
}

/// 一名员工的人事快照（明细行要存的那几列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmployeeHead {
    /// 员工 id。
    pub id: Uuid,
    /// 工号。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 部门 id。
    pub dept_id: Option<Uuid>,
    /// 部门名（join `sys_dept`；join 不到为空）。
    pub dept_name: Option<String>,
    /// 关联账号（数据权限 SELF 的锚点）。
    pub user_id: Option<Uuid>,
    /// 岗位。
    pub position_code: Option<String>,
    /// 职级。
    pub job_level: Option<String>,
}

/// 一条当期生效的薪资标准。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardRow {
    /// 员工 id。
    pub employee_id: Uuid,
    /// 薪资项编码。
    pub item_code: String,
    /// 金额（月标准）。
    pub amount: Decimal,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
}

/// 一人的全部取数（试算的输入快照）。
#[derive(Debug, Clone)]
pub struct PayInput {
    /// 人事快照。
    pub head: EmployeeHead,
    /// 当期生效的薪资标准（`item_code → 金额`）。
    pub standard: BTreeMap<String, Decimal>,
    /// 当期考勤（`None` = 这一期没有这个人的考勤记录）。
    pub attendance: Option<AttendanceNumbers>,
    /// 当期考勤记录 id（留痕 / 排错）。
    pub attendance_record_id: Option<Uuid>,
    /// 人工录入（`item_code → 金额`）。
    pub manual: BTreeMap<String, Decimal>,
}

impl PayInput {
    /// 只带人事快照的空输入（单测 / 适配用）。
    pub fn new(head: EmployeeHead) -> Self {
        Self {
            head,
            standard: BTreeMap::new(),
            attendance: None,
            attendance_record_id: None,
            manual: BTreeMap::new(),
        }
    }
}

/// 一个人的一项结果（= 一行 `biz_salary_detail_item`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayslipItem {
    /// 薪资项编码。
    pub item_code: String,
    /// 薪资项名称（快照）。
    pub item_name: String,
    /// 项类型（`EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER`）。
    pub item_type: String,
    /// 算法（`FIXED` / `FORMULA` / `INPUT`）。
    pub calc_type: String,
    /// 金额（公式项 2 位，其余 4 位收口）。
    pub amount: Decimal,
    /// 计算时的公式原文（快照：改公式不改历史）。
    pub expr_snapshot: Option<String>,
    /// 代入值说明（形如 `8000 / 21.75 / 8 * 10 * 1.5 = 689.66`）。
    pub calc_note: String,
    /// 行号（拓扑序 + 1，与配置页顺序一致）。
    pub line_no: i32,
}

/// 一个人的工资条（= 一行 `biz_salary_detail` + 它的明细项）。
#[derive(Debug, Clone)]
pub struct Payslip {
    /// 人事快照。
    pub head: EmployeeHead,
    /// 考勤快照（已收口到 2 位）。
    pub attendance: AttendanceNumbers,
    /// 考勤记录 id。
    pub attendance_record_id: Option<Uuid>,
    /// 逐项结果（**不含** `SUMMARY` 项）。
    pub items: Vec<PayslipItem>,
    /// Σ `EARNING`。
    pub earning_amount: Decimal,
    /// Σ `PRE_DEDUCT`。
    pub pre_deduct_amount: Decimal,
    /// 应发。
    pub gross_amount: Decimal,
    /// 社保（个人）。
    pub social_personal: Decimal,
    /// 公积金（个人）。
    pub fund_personal: Decimal,
    /// 个税。
    pub iit_amount: Decimal,
    /// 其他代扣。
    pub other_withhold: Decimal,
    /// 代扣合计。
    pub withhold_amount: Decimal,
    /// 实发。
    pub net_amount: Decimal,
    /// 单位社保。
    pub si_company: Decimal,
    /// 单位公积金。
    pub hf_company: Decimal,
    /// 明细行备注（当期无考勤记录时写明）。
    pub remark: Option<String>,
}

impl Payslip {
    /// 计提总额 = 应发 + 单位社保 + 单位公积金（设计 §4.4.1 第四条恒等式）。
    pub fn accrual_amount(&self) -> Decimal {
        self.gross_amount + self.si_company + self.hf_company
    }

    /// 这一行落库前的自检（逐人恒等式）。
    pub fn check(&self) -> std::result::Result<(), Vec<CalcIssue>> {
        check_identity(self)
    }
}

/// 一张薪资单的合计（= 单头的 7 个金额列 + 人数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CalcTotals {
    /// 人数。
    pub employee_count: i32,
    /// Σ EARNING。
    pub earning_total: Decimal,
    /// Σ PRE_DEDUCT。
    pub pre_deduct_total: Decimal,
    /// Σ 应发。
    pub gross_total: Decimal,
    /// Σ 代扣。
    pub withhold_total: Decimal,
    /// Σ 实发。
    pub net_total: Decimal,
    /// Σ 单位社保。
    pub si_company_total: Decimal,
    /// Σ 单位公积金。
    pub hf_company_total: Decimal,
}

impl CalcTotals {
    /// 计提总额（计提凭证借方合计，设计 §4.6.1）。
    pub fn accrual_total(&self) -> Decimal {
        self.gross_total + self.si_company_total + self.hf_company_total
    }

    /// 从逐人工资条汇总。
    pub fn of(slips: &[Payslip]) -> Self {
        let mut totals = Self {
            employee_count: i32::try_from(slips.len()).unwrap_or(i32::MAX),
            ..Self::default()
        };
        for slip in slips {
            totals.earning_total += slip.earning_amount;
            totals.pre_deduct_total += slip.pre_deduct_amount;
            totals.gross_total += slip.gross_amount;
            totals.withhold_total += slip.withhold_amount;
            totals.net_total += slip.net_amount;
            totals.si_company_total += slip.si_company;
            totals.hf_company_total += slip.hf_company;
        }
        totals
    }
}

/// 落库后的单头引用（试算时为空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetRef {
    /// 单头 id。
    pub id: Uuid,
    /// 单号（`SAL-yyyyMM-NNNNNN`）。
    pub sheet_no: String,
    /// 落库后的状态（一期固定 `CALCULATED`）。
    pub status: String,
}

/// 试算 / 落库的结果。
#[derive(Debug, Clone)]
pub struct CalcOutcome {
    /// 薪资期间 `yyyy-MM`。
    pub period_code: String,
    /// 期间起（含）。
    pub period_start: NaiveDate,
    /// 期间止（含）。
    pub period_end: NaiveDate,
    /// 数据来源考勤期间（解析后的值：给了 id 用 id，没给按期间码找）。
    pub attendance_period_id: Option<Uuid>,
    /// 是否只试算。
    pub dry_run: bool,
    /// 合计。
    pub totals: CalcTotals,
    /// 逐人工资条（按工号升序）。
    pub slips: Vec<Payslip>,
    /// 落库的单头（`dry_run = true` 时为 `None`）。
    pub sheet: Option<SheetRef>,
    /// 进了表但**没有当期考勤记录**的员工（`工号 姓名`）。
    pub missing_attendance: Vec<String>,
    /// 参与计算的薪资项个数。
    pub item_count: usize,
}

/// 一条人工录入（试算入参）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualEntry {
    /// 员工 id。
    pub employee_id: Uuid,
    /// 薪资项编码（必须是启用中的 `INPUT` 项）。
    pub item_code: String,
    /// 金额（可负，见设计 §4.8.5）。
    pub amount: Decimal,
}

/// 试算 / 建单的入参（设计 §5.2 #39/#40）。
#[derive(Debug, Clone)]
pub struct CalcRequest {
    /// 薪资期间 `yyyy-MM`。
    pub period_code: String,
    /// 数据来源考勤期间 id（`None` = 按 `period_code` 找当期期间，找不到就当没有考勤）。
    pub attendance_period_id: Option<Uuid>,
    /// `true` = 只试算不落库。
    pub dry_run: bool,
    /// 人工录入项（社保 / 公积金 / 个税 / 绩效 / 补发 …）。
    pub manual: Vec<ManualEntry>,
    /// 备注（写单头 `remark`）。
    pub remark: Option<String>,
}

impl CalcRequest {
    /// 只试算的入参。
    pub fn preview(period_code: impl Into<String>, attendance_period_id: Option<Uuid>) -> Self {
        Self {
            period_code: period_code.into(),
            attendance_period_id,
            dry_run: true,
            manual: Vec::new(),
            remark: None,
        }
    }

    /// 建单落库的入参（同一次计算的 `dry_run = false`）。
    pub fn persist(period_code: impl Into<String>, attendance_period_id: Option<Uuid>) -> Self {
        Self {
            dry_run: false,
            ..Self::preview(period_code, attendance_period_id)
        }
    }
}

/// 薪资项的「全列元数据」（[`ItemSpec`] 只有解析变量用得上的列，`item_type` / `id` 在这里）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMeta {
    /// 主键（落 `detail_item.item_id`）。
    pub id: Option<Uuid>,
    /// 编码。
    pub item_code: String,
    /// 名称。
    pub item_name: String,
    /// 类型（五类之一）。
    pub item_type: String,
    /// 算法（`FIXED` / `FORMULA` / `INPUT`）。
    pub calc_type: String,
    /// 公式原文。
    pub formula: Option<String>,
    /// 贷方科目（`EMPLOYER` 项靠它判「单位社保 / 单位公积金」，见 [`employer_bucket`]）。
    pub credit_account_code: Option<String>,
}

impl ItemMeta {
    /// 造一条（单测用）。
    pub fn new(
        item_code: impl Into<String>,
        item_name: impl Into<String>,
        item_type: impl Into<String>,
        calc_type: impl Into<String>,
        formula: Option<&str>,
    ) -> Self {
        Self {
            id: None,
            item_code: item_code.into(),
            item_name: item_name.into(),
            item_type: item_type.into(),
            calc_type: calc_type.into(),
            formula: formula.map(str::to_string),
            credit_account_code: None,
        }
    }

    /// 带上贷方科目（单位承担项靠科目归位，单测用）。
    pub fn with_credit(mut self, credit_account_code: Option<&str>) -> Self {
        self.credit_account_code = credit_account_code.map(str::to_string);
        self
    }

    /// 从 DB 行投影。
    pub fn of_row(row: &SalaryItemRow) -> Self {
        Self {
            id: Some(row.id),
            item_code: row.item_code.clone(),
            item_name: row.item_name.clone(),
            item_type: row.item_type.clone(),
            calc_type: row.calc_type.clone(),
            formula: row.formula.clone(),
            credit_account_code: row.credit_account_code.clone(),
        }
    }

    /// 是否汇总项（不落明细项）。
    pub fn is_summary(&self) -> bool {
        self.item_type == ITEM_TYPE_SUMMARY
    }
}

// ==================== 引擎 ====================

/// 计算引擎：**一份**解析好的计划 + 项元数据（编码 → 全列）。
///
/// 为什么把两者绑在一起：`salary_vars::ComputePlan` 只有「公式怎么算」，
/// 而落库要的「这一项是 EARNING 还是 WITHHOLD」「item_id 是多少」在原行上。
/// 引擎在 [`Self::of_rows`] 里一次建好，逐人计算时零查库。
#[derive(Debug, Clone)]
pub struct Engine {
    plan: ComputePlan,
    metas: BTreeMap<String, ItemMeta>,
}

impl Engine {
    /// 从 DB 行建引擎（HR-33 的 `plan_from_rows` + 全列投影）。
    pub fn of_rows(rows: &[SalaryItemRow]) -> std::result::Result<Self, Vec<VarIssue>> {
        let specs = spec_of_rows(rows);
        let plan = salary_vars::build_plan(&specs)?;
        let metas = rows
            .iter()
            .map(|row| (row.item_code.clone(), ItemMeta::of_row(row)))
            .collect();
        Ok(Self { plan, metas })
    }

    /// 从「计划 + 元数据」建引擎（单测 / 上游已建好计划时用）。
    pub fn new(plan: ComputePlan, metas: impl IntoIterator<Item = ItemMeta>) -> Self {
        Self {
            plan,
            metas: metas.into_iter().map(|meta| (meta.item_code.clone(), meta)).collect(),
        }
    }

    /// 计算计划（拓扑序 / 取数清单）。
    pub fn plan(&self) -> &ComputePlan {
        &self.plan
    }

    /// 取一项的元数据。
    pub fn item(&self, item_code: &str) -> Option<&ItemMeta> {
        self.metas.get(item_code)
    }

    /// 一个启用项都没有。
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// 参与计算的项数。
    pub fn len(&self) -> usize {
        self.plan.len()
    }

    /// 要落 `detail_item` 的项数（`SUMMARY` 不落）。
    pub fn detail_item_count(&self) -> usize {
        self.plan
            .order()
            .iter()
            .filter(|code| !self.item(code).map(ItemMeta::is_summary).unwrap_or(false))
            .count()
    }

    /// 算一个人（纯函数，不连库）。
    ///
    /// # 步骤
    ///
    /// 1. `FIXED` → 取薪资标准（缺 → 0）；`INPUT` → 取人工录入（缺 → 0）；
    /// 2. 考勤变量按 [`ComputePlan::att_bindings`] 的取数清单灌进作用域（无考勤记录 → 全 0）；
    /// 3. 按拓扑序逐项求值（**一个公式算 N 个人**：语法树来自计划，不重复解析）；
    /// 4. 按 `item_type` 聚合出应发 / 代扣 / 实发 + 四个代扣桶 + 两个单位桶；
    /// 5. [`check_identity`] 复算恒等式 + `GROSS` / `NET` 公式交叉校验。
    pub fn compute(&self, input: &PayInput) -> std::result::Result<Payslip, Vec<CalcIssue>> {
        let head = &input.head;
        let mut issues: Vec<CalcIssue> = Vec::new();
        let attendance = input.attendance.unwrap_or_default().round_snapshot();

        // ---- ① 作用域：考勤变量 + 固定项 + 人工项 ----
        let mut scope: BTreeMap<String, Decimal> = BTreeMap::new();
        for binding in self.plan.att_bindings() {
            if let Some(value) = attendance.value_of(binding.column) {
                scope.insert(binding.var.clone(), value);
            }
        }
        for code in self.plan.order() {
            let Some(item) = self.plan.get(code) else {
                continue;
            };
            match item.spec.calc_type.as_str() {
                CALC_TYPE_FIXED => {
                    let amount = input.standard.get(code).copied().unwrap_or(Decimal::ZERO);
                    scope.insert(code.clone(), round_amount(amount));
                }
                CALC_TYPE_INPUT => {
                    let amount = input.manual.get(code).copied().unwrap_or(Decimal::ZERO);
                    scope.insert(code.clone(), round_amount(amount));
                }
                _ => {}
            }
        }

        // ---- ② 公式项：拓扑序逐项求值（出错点名「哪个人哪一项」） ----
        for code in self.plan.order() {
            let Some(item) = self.plan.get(code) else {
                continue;
            };
            let Some(expr) = item.expr.as_ref() else {
                continue;
            };
            match expr.eval_number(&scope) {
                Ok(value) => {
                    scope.insert(code.clone(), value);
                }
                Err(error) => {
                    // 一项算不出来，依赖它的项只会连环报「未定义变量」→ 一个人只报第一处
                    return Err(vec![CalcIssue::Formula {
                        emp_no: head.emp_no.clone(),
                        item_code: code.clone(),
                        message: error.message().to_string(),
                    }]);
                }
            }
        }

        // ---- ③ 逐项落行（`SUMMARY` 不落明细项，设计 §4.4.3 末尾注） ----
        let mut items: Vec<PayslipItem> = Vec::with_capacity(self.plan.len());
        let mut line_no: i32 = 0;
        for code in self.plan.order() {
            let Some(item) = self.plan.get(code) else {
                continue;
            };
            if self.item(code).map(ItemMeta::is_summary).unwrap_or(false) {
                continue;
            }
            let amount = round_amount(scope.get(code).copied().unwrap_or(Decimal::ZERO));
            line_no += 1;
            let calc_note = match item.spec.calc_type.as_str() {
                CALC_TYPE_FIXED => note_fixed(&input.standard, code, amount),
                CALC_TYPE_INPUT => note_input(&input.manual, code, amount),
                _ => note_formula(item.expr.as_ref(), &scope, amount),
            };
            items.push(PayslipItem {
                item_code: code.clone(),
                item_name: item.spec.item_name.clone(),
                item_type: self.item(code).map(|meta| meta.item_type.clone()).unwrap_or_default(),
                calc_type: item.spec.calc_type.clone(),
                amount,
                expr_snapshot: item.spec.formula.clone().map(|text| truncate(&text, NOTE_MAX_LEN)),
                calc_note: truncate(&calc_note, NOTE_MAX_LEN),
                line_no,
            });
        }

        // ---- ④ 按类型聚合 ----
        let mut earning = Decimal::ZERO;
        let mut pre_deduct = Decimal::ZERO;
        let mut social = Decimal::ZERO;
        let mut fund = Decimal::ZERO;
        let mut iit = Decimal::ZERO;
        let mut other = Decimal::ZERO;
        let mut si_company = Decimal::ZERO;
        let mut hf_company = Decimal::ZERO;
        for item in &items {
            match item.item_type.as_str() {
                ITEM_TYPE_EARNING => earning += item.amount,
                ITEM_TYPE_PRE_DEDUCT => pre_deduct += item.amount,
                ITEM_TYPE_WITHHOLD => {
                    if item.amount < Decimal::ZERO {
                        issues.push(CalcIssue::NegativeAmount {
                            emp_no: head.emp_no.clone(),
                            item_code: item.item_code.clone(),
                            amount: item.amount,
                        });
                    }
                    // 归桶照算（否则「代扣 = 四桶之和」这条恒等式会因为跳过负数而假红）
                    match withhold_bucket(&item.item_code) {
                        WithholdBucket::Social => social += item.amount,
                        WithholdBucket::Fund => fund += item.amount,
                        WithholdBucket::Iit => iit += item.amount,
                        WithholdBucket::Other => other += item.amount,
                    }
                }
                ITEM_TYPE_EMPLOYER => {
                    if item.amount < Decimal::ZERO {
                        issues.push(CalcIssue::NegativeAmount {
                            emp_no: head.emp_no.clone(),
                            item_code: item.item_code.clone(),
                            amount: item.amount,
                        });
                    }
                    match employer_bucket(
                        &item.item_code,
                        self.item(&item.item_code)
                            .and_then(|meta| meta.credit_account_code.as_deref()),
                    ) {
                        EmployerBucket::Social => si_company += item.amount,
                        EmployerBucket::Fund => hf_company += item.amount,
                        EmployerBucket::Unclassified => {
                            if item.amount != Decimal::ZERO {
                                issues.push(CalcIssue::UnclassifiedEmployer {
                                    emp_no: head.emp_no.clone(),
                                    item_code: item.item_code.clone(),
                                    item_name: item.item_name.clone(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let earning_amount = round_amount(earning);
        let pre_deduct_amount = round_amount(pre_deduct);
        let gross_amount = round_amount(earning_amount - pre_deduct_amount);
        let withhold_amount = round_amount(social + fund + iit + other);
        let net_amount = round_amount(gross_amount - withhold_amount);

        let slip = Payslip {
            head: head.clone(),
            attendance,
            attendance_record_id: input.attendance_record_id,
            items,
            earning_amount,
            pre_deduct_amount,
            gross_amount,
            social_personal: round_amount(social),
            fund_personal: round_amount(fund),
            iit_amount: round_amount(iit),
            other_withhold: round_amount(other),
            withhold_amount,
            net_amount,
            si_company: round_amount(si_company),
            hf_company: round_amount(hf_company),
            remark: input
                .attendance
                .is_none()
                .then(|| "当期无考勤记录：考勤变量按 0 计，请补录考勤后重算".to_string()),
        };

        // ---- ⑤ 恒等式 + 公式项交叉校验 ----
        if let Err(mut more) = check_identity(&slip) {
            issues.append(&mut more);
        }
        for (code, aggregate, what) in [
            (SUMMARY_GROSS_CODE, slip.gross_amount, "应发"),
            (SUMMARY_NET_CODE, slip.net_amount, "实发"),
        ] {
            if let Err(mut more) = self.check_summary(&slip, &scope, code, aggregate, what) {
                issues.append(&mut more);
            }
        }

        if issues.is_empty() { Ok(slip) } else { Err(issues) }
    }

    /// `SUMMARY` 项的公式值 vs 按类型聚合值。
    ///
    /// 不相等只有一种成因：项的 `item_type` 配错了（例如把考勤扣款配成 `EARNING`，
    /// 于是聚合侧把它加上去、公式侧减下去）。库层 `CHECK` 只看聚合值，看不出这种错。
    pub fn check_summary(
        &self,
        slip: &Payslip,
        scope: &BTreeMap<String, Decimal>,
        code: &str,
        aggregate: Decimal,
        what: &'static str,
    ) -> std::result::Result<(), Vec<CalcIssue>> {
        let Some(item) = self.plan.get(code) else {
            return Ok(());
        };
        if item.expr.is_none() {
            // 没有 GROSS / NET 公式项（客户把系统项停用了）：聚合值照样成立，不报
            return Ok(());
        }
        let Some(value) = scope.get(code).copied() else {
            return Ok(());
        };
        if round_amount(value) == aggregate {
            return Ok(());
        }
        let label: &'static str = if what == "应发" {
            "应发合计（公式项 GROSS 与按项类型聚合）"
        } else {
            "实发合计（公式项 NET 与按项类型聚合）"
        };
        Err(vec![CalcIssue::Identity {
            emp_no: slip.head.emp_no.clone(),
            name: slip.head.name.clone(),
            what: label,
            expected: aggregate,
            actual: round_amount(value),
        }])
    }

    /// 一组人的工资条（[`Self::compute`] 的批量版）。
    ///
    /// 一个人算不出来**不影响**别人被算出来（问题全部收集、一次返回，配置错一轮改完），
    /// 但只要有一个人的问题就整体失败 —— 半张工资表比没算更危险。
    pub fn compute_all(&self, inputs: &[PayInput]) -> std::result::Result<Vec<Payslip>, Vec<CalcIssue>> {
        if self.is_empty() {
            return Err(vec![CalcIssue::NoActiveItems]);
        }
        let mut slips: Vec<Payslip> = Vec::with_capacity(inputs.len());
        let mut issues: Vec<CalcIssue> = Vec::new();
        for input in inputs {
            match self.compute(input) {
                Ok(slip) => slips.push(slip),
                Err(mut more) => issues.append(&mut more),
            }
        }
        if issues.is_empty() { Ok(slips) } else { Err(issues) }
    }

    /// 人工录入的静态校验：编码必须存在且是 `INPUT` 项。
    ///
    /// 放在计算之前跑：「录错了项编码 / 把基本工资手工填了」不该等算完 200 人才报。
    pub fn validate_manual(
        &self,
        manual: &[ManualEntry],
        employees: &[EmployeeHead],
    ) -> std::result::Result<(), Vec<CalcIssue>> {
        let mut issues: Vec<CalcIssue> = Vec::new();
        let mut seen: BTreeSet<(Uuid, &str)> = BTreeSet::new();
        for entry in manual {
            let Some(item) = self.plan.get(&entry.item_code) else {
                let issue = CalcIssue::UnknownManualItem {
                    item_code: entry.item_code.clone(),
                };
                if !issues.contains(&issue) {
                    issues.push(issue);
                }
                continue;
            };
            if item.spec.calc_type != CALC_TYPE_INPUT {
                issues.push(CalcIssue::ManualNotAllowed {
                    emp_no: emp_no_of(employees, entry.employee_id),
                    item_code: entry.item_code.clone(),
                    calc_type: item.spec.calc_type.clone(),
                });
                continue;
            }
            if !seen.insert((entry.employee_id, entry.item_code.as_str())) {
                issues.push(CalcIssue::DuplicateManualEntry {
                    emp_no: emp_no_of(employees, entry.employee_id),
                    item_code: entry.item_code.clone(),
                });
            }
        }
        if issues.is_empty() { Ok(()) } else { Err(issues) }
    }
}

/// DB 行 → [`ItemSpec`]（顺序即调用方给的 `sort_order`）。
pub fn spec_of_rows(rows: &[SalaryItemRow]) -> Vec<ItemSpec> {
    rows.iter()
        .map(|row| ItemSpec {
            item_code: row.item_code.clone(),
            item_name: row.item_name.clone(),
            calc_type: row.calc_type.clone(),
            formula: row.formula.clone(),
            is_active: row.is_active == 1,
        })
        .collect()
}

// ==================== 纯函数：金额 / 期间 ====================

/// `ROUND_HALF_UP` 到 `scale` 位。
pub fn round_to(value: Decimal, scale: u32) -> Decimal {
    value.round_dp_with_strategy(scale, ROUNDING)
}

/// 金额列收口（`NUMERIC(18,4)`）。
pub fn round_amount(value: Decimal) -> Decimal {
    round_to(value, AMOUNT_SCALE)
}

/// 解析薪资期间 `yyyy-MM` → `(期初, 期末)`。
///
/// 期末 = 「下月 1 号减一天」，闰年 / 大小月由 `pred_opt` 自己算对。
pub fn period_range(period_code: &str) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let text = period_code.trim();
    let mut parts = text.split('-');
    let (Some(year), Some(month), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad_period(period_code));
    };
    if year.len() != 4 || month.len() != 2 {
        return Err(bad_period(period_code));
    }
    let (Ok(year), Ok(month)) = (year.parse::<i32>(), month.parse::<u32>()) else {
        return Err(bad_period(period_code));
    };
    if !(1..=12).contains(&month) {
        return Err(bad_period(period_code));
    }
    let start = NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(|| bad_period(period_code))?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(|| bad_period(period_code))?;
    let end = next.pred_opt().ok_or_else(|| bad_period(period_code))?;
    Ok((start, end))
}

/// 期间码格式错 → 400。
pub fn bad_period(period_code: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!("薪资期间 `{period_code}` 不合法：格式必须是 `yyyy-MM`（例如 `2026-11`）"),
    )
}

/// 取号用的周期键：按 `biz_seq.date_pattern` 把薪资期间摊成键（`SAL` 是 `yyyyMM`）。
///
/// 与 `erp::voucher::next_doc_no`（按**当天**取号）的区别：薪资单号里的日期段是
/// **薪资期间**（`SAL-202611-000001`），不是建单当天 —— 补做 10 月的工资单，
/// 单号必须是 `SAL-202610-*`，否则财务按号对不上账。
///
/// `ck_biz_seq_date`（`V0386`）只允许 `yyyyMMdd` / `yyyyMM` / `none` 三值，别的一律按 `yyyyMM`。
pub fn period_key_for(date_pattern: &str, start: NaiveDate, end: NaiveDate) -> String {
    match date_pattern.trim() {
        "none" => "GLOBAL".to_string(),
        "yyyyMMdd" => end.format("%Y%m%d").to_string(),
        _ => start.format("%Y%m").to_string(),
    }
}

/// 组装单号：`{prefix}{sep}{period_key}{sep}{no:0width$}`。
pub fn format_sheet_no(prefix: &str, separator: &str, period_key: &str, pad_width: i32, no: i64) -> String {
    let width = usize::try_from(pad_width.max(1)).unwrap_or(6);
    format!("{prefix}{separator}{period_key}{separator}{no:0width$}")
}

/// 明细项代扣金额 → 四个桶（`social_personal` / `fund_personal` / `iit_amount` / `other_withhold`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithholdBucket {
    /// 社保个人。
    Social,
    /// 公积金个人。
    Fund,
    /// 个税。
    Iit,
    /// 其他代扣（兜底：列名叫 `other_withhold`）。
    Other,
}

/// 按编码把 `WITHHOLD` 项归类到明细的四个列。
pub fn withhold_bucket(item_code: &str) -> WithholdBucket {
    match item_code {
        WITHHOLD_SOCIAL_CODE => WithholdBucket::Social,
        WITHHOLD_FUND_CODE => WithholdBucket::Fund,
        WITHHOLD_IIT_CODE => WithholdBucket::Iit,
        _ => WithholdBucket::Other,
    }
}

/// 明细项单位承担金额 → 两个桶（`si_company` / `hf_company`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmployerBucket {
    /// 单位社保。
    Social,
    /// 单位公积金。
    Fund,
    /// 没有对应列 → [`CalcIssue::UnclassifiedEmployer`]。
    Unclassified,
}

/// 单位社保 / 单位公积金对应的**负债科目**（设计 §3.2 ① 的 9 个末级科目）。
///
/// `2211.02 应付职工薪酬-社保(单位)` 与 `2211.03 应付职工薪酬-公积金(单位)` ——
/// 明细的两列在会计上就是这两个负债，所以科目是比编码更本质的判据。
pub const ACCOUNT_SI_COMPANY: &str = "2211.02";
/// 单位公积金负债科目。
pub const ACCOUNT_HF_COMPANY: &str = "2211.03";

/// 把 `EMPLOYER` 项归类到明细的两个单位列。
///
/// # 判据顺序（先编码、再科目）
///
/// 1. 编码是种子项 `SI_COMPANY` / `HF_COMPANY` → 直接归位（一期 17 个种子项走这条）；
/// 2. 否则看该项的**贷方科目**：`2211.02` → 单位社保、`2211.03` → 单位公积金
///    （客户把编码改成 `UNION_FEE` 之外的社保项、或自建 `SI_COMPANY_2` 时仍能归位）；
/// 3. 都不中 → [`EmployerBucket::Unclassified`]，由调用方报错点名（不猜列）。
pub fn employer_bucket(item_code: &str, credit_account_code: Option<&str>) -> EmployerBucket {
    match item_code {
        EMPLOYER_SOCIAL_CODE => EmployerBucket::Social,
        EMPLOYER_FUND_CODE => EmployerBucket::Fund,
        _ => match credit_account_code.map(str::trim) {
            Some(ACCOUNT_SI_COMPANY) => EmployerBucket::Social,
            Some(ACCOUNT_HF_COMPANY) => EmployerBucket::Fund,
            _ => EmployerBucket::Unclassified,
        },
    }
}

// ==================== 纯函数：恒等式 / 取数拼装 ====================

/// 逐人恒等式校验（**落库前的最后一道人话闸**）。
///
/// 1. `应发 = Σ EARNING − Σ PRE_DEDUCT`
/// 2. `代扣 = 社保个人 + 公积金个人 + 个税 + 其他代扣`
/// 3. `实发 = 应发 − 代扣`（且各金额列非负，库层 `CHECK` 同款）
pub fn check_identity(slip: &Payslip) -> std::result::Result<(), Vec<CalcIssue>> {
    let mut issues: Vec<CalcIssue> = Vec::new();
    let identity = |what: &'static str, expected: Decimal, actual: Decimal| CalcIssue::Identity {
        emp_no: slip.head.emp_no.clone(),
        name: slip.head.name.clone(),
        what,
        expected,
        actual,
    };

    let gross_expected = round_amount(slip.earning_amount - slip.pre_deduct_amount);
    if gross_expected != slip.gross_amount {
        issues.push(identity(
            "应发恒等式（应发 = 收入 − 应发前扣减）",
            gross_expected,
            slip.gross_amount,
        ));
    }

    let withhold_expected =
        round_amount(slip.social_personal + slip.fund_personal + slip.iit_amount + slip.other_withhold);
    if withhold_expected != slip.withhold_amount {
        issues.push(identity(
            "代扣恒等式（代扣 = 社保 + 公积金 + 个税 + 其他）",
            withhold_expected,
            slip.withhold_amount,
        ));
    }

    let net_expected = round_amount(slip.gross_amount - slip.withhold_amount);
    if net_expected != slip.net_amount {
        issues.push(identity(
            "实发恒等式（实发 = 应发 − 代扣）",
            net_expected,
            slip.net_amount,
        ));
    }

    // 库层 `ck_biz_sal_det_nonneg` 要求 10 个金额列都 ≥ 0：负数在这里先说清「谁、多少、怎么办」
    if slip.earning_amount < Decimal::ZERO || slip.pre_deduct_amount < Decimal::ZERO {
        issues.push(identity(
            "收入 / 应发前扣减非负",
            Decimal::ZERO,
            slip.earning_amount.min(slip.pre_deduct_amount),
        ));
    }
    if slip.gross_amount < Decimal::ZERO {
        issues.push(CalcIssue::NegativeGross {
            emp_no: slip.head.emp_no.clone(),
            name: slip.head.name.clone(),
            gross: slip.gross_amount,
            earning: slip.earning_amount,
            pre_deduct: slip.pre_deduct_amount,
        });
    }
    if slip.net_amount < Decimal::ZERO {
        issues.push(CalcIssue::NegativeNet {
            emp_no: slip.head.emp_no.clone(),
            name: slip.head.name.clone(),
            net: slip.net_amount,
            gross: slip.gross_amount,
            withhold: slip.withhold_amount,
        });
    }
    if slip.si_company < Decimal::ZERO || slip.hf_company < Decimal::ZERO {
        issues.push(identity(
            "单位承担项非负",
            Decimal::ZERO,
            slip.si_company.min(slip.hf_company),
        ));
    }

    if issues.is_empty() { Ok(()) } else { Err(issues) }
}

/// 把「员工 + 标准 + 考勤 + 人工录入」拼成逐人取数快照。
///
/// 标准区间重叠在这里拦（[`CalcIssue::StandardOverlap`]）：同一人同一项两条都覆盖当期，
/// `ORDER BY` 选谁都是随机数（设计 §3.1 ③ 的原话），必须在试算前挡下来。
pub fn assemble(
    employees: &[EmployeeHead],
    standards: &[StandardRow],
    attendance: &BTreeMap<Uuid, (Uuid, AttendanceNumbers)>,
    manual: &[ManualEntry],
) -> std::result::Result<Vec<PayInput>, Vec<CalcIssue>> {
    let mut issues: Vec<CalcIssue> = Vec::new();

    // ---- 标准：按 (员工, 项) 分组，检出重叠 ----
    let mut grouped: BTreeMap<(&Uuid, &str), Vec<&StandardRow>> = BTreeMap::new();
    for row in standards {
        grouped
            .entry((&row.employee_id, row.item_code.as_str()))
            .or_default()
            .push(row);
    }
    let mut per_employee: BTreeMap<Uuid, BTreeMap<String, Decimal>> = BTreeMap::new();
    for ((employee_id, item_code), rows) in grouped {
        if rows.len() > 1 {
            let mut days: Vec<NaiveDate> = rows.iter().map(|row| row.effective_from).collect();
            days.sort_unstable();
            issues.push(CalcIssue::StandardOverlap {
                emp_no: emp_no_of(employees, *employee_id),
                item_code: item_code.to_string(),
                effective_from: days,
            });
            continue;
        }
        per_employee
            .entry(*employee_id)
            .or_default()
            .insert(item_code.to_string(), round_amount(rows[0].amount));
    }

    // ---- 人工录入：按员工分组（未知员工也在这里报出来） ----
    let known: BTreeSet<Uuid> = employees.iter().map(|head| head.id).collect();
    let mut manual_map: BTreeMap<Uuid, BTreeMap<String, Decimal>> = BTreeMap::new();
    let mut reported_unknown: BTreeSet<Uuid> = BTreeSet::new();
    for entry in manual {
        if !known.contains(&entry.employee_id) {
            if reported_unknown.insert(entry.employee_id) {
                issues.push(CalcIssue::UnknownManualEmployee {
                    employee_id: entry.employee_id.to_string(),
                });
            }
            continue;
        }
        let bucket = manual_map.entry(entry.employee_id).or_default();
        if bucket
            .insert(entry.item_code.clone(), round_amount(entry.amount))
            .is_some()
        {
            issues.push(CalcIssue::DuplicateManualEntry {
                emp_no: emp_no_of(employees, entry.employee_id),
                item_code: entry.item_code.clone(),
            });
        }
    }

    let mut inputs: Vec<PayInput> = Vec::with_capacity(employees.len());
    for head in employees {
        let (record_id, numbers) = match attendance.get(&head.id) {
            Some((id, numbers)) => (Some(*id), Some(*numbers)),
            None => (None, None),
        };
        inputs.push(PayInput {
            head: head.clone(),
            standard: per_employee.remove(&head.id).unwrap_or_default(),
            attendance: numbers,
            attendance_record_id: record_id,
            manual: manual_map.remove(&head.id).unwrap_or_default(),
        });
    }

    if issues.is_empty() { Ok(inputs) } else { Err(issues) }
}

/// `FIXED` 项的代入说明。
fn note_fixed(standard: &BTreeMap<String, Decimal>, code: &str, amount: Decimal) -> String {
    match standard.get(code) {
        Some(value) => format!("取薪资标准 {} = {amount}", plain(*value)),
        None => "当期未配薪资标准（按 0 计）".to_string(),
    }
}

/// `INPUT` 项的代入说明。
fn note_input(manual: &BTreeMap<String, Decimal>, code: &str, amount: Decimal) -> String {
    match manual.get(code) {
        Some(value) => format!("人工录入 {} = {amount}", plain(*value)),
        None => "未录入（按 0 计）".to_string(),
    }
}

/// `FORMULA` 项的代入说明：把变量换成实际值，再写结果（设计 §3.1 ⑩ 的 `calc_note` 例）。
fn note_formula(expr: Option<&Expr>, scope: &BTreeMap<String, Decimal>, amount: Decimal) -> String {
    match expr {
        Some(expr) => format!("{} = {amount}", render(expr, scope)),
        None => format!("公式缺失（按 0 计）= {amount}"),
    }
}

/// 把语法树渲染成「代入值之后」的算式（`8000 / 21.75 / 8 * 10 * 1.5`）。
///
/// 只给 `calc_note`（给人看的解释层）用：不做任何精度决定，不参与求值。
/// 复合子式**加括号**，避免 `(a + b) * c` 被渲染成 `a + b * c` 让人误读。
fn render(expr: &Expr, scope: &BTreeMap<String, Decimal>) -> String {
    match expr {
        Expr::Num(value) => plain(*value),
        Expr::Var(name) => match scope.get(name) {
            Some(value) => plain(*value),
            None => format!("{{{name}}}"),
        },
        Expr::Neg(inner) => format!("-{}", wrap(inner, scope)),
        Expr::Bin(op, left, right) => {
            let symbol = match op {
                salary_formula::BinOp::Add => "+",
                salary_formula::BinOp::Sub => "-",
                salary_formula::BinOp::Mul => "*",
                salary_formula::BinOp::Div => "/",
            };
            format!("{} {symbol} {}", wrap(left, scope), wrap(right, scope))
        }
        Expr::Cmp(op, left, right) => {
            let symbol = match op {
                salary_formula::CmpOp::Eq => "==",
                salary_formula::CmpOp::Ne => "!=",
                salary_formula::CmpOp::Gt => ">",
                salary_formula::CmpOp::Ge => ">=",
                salary_formula::CmpOp::Lt => "<",
                salary_formula::CmpOp::Le => "<=",
            };
            format!("{} {symbol} {}", wrap(left, scope), wrap(right, scope))
        }
        Expr::Logic(op, left, right) => {
            let symbol = match op {
                salary_formula::LogicOp::And => "&&",
                salary_formula::LogicOp::Or => "||",
            };
            format!("{} {symbol} {}", wrap(left, scope), wrap(right, scope))
        }
        Expr::Call(func, args) => {
            let rendered: Vec<String> = args.iter().map(|arg| render(arg, scope)).collect();
            format!("{}({})", func.name(), rendered.join(", "))
        }
    }
}

/// 复合子式渲染时加括号（数字与变量不加）。
fn wrap(expr: &Expr, scope: &BTreeMap<String, Decimal>) -> String {
    match expr {
        Expr::Num(_) | Expr::Var(_) => render(expr, scope),
        _ => format!("({})", render(expr, scope)),
    }
}

/// `Decimal` 的人类可读形态（去掉无意义尾零：`689.66` / `200`）。
fn plain(value: Decimal) -> String {
    let text = value.normalize().to_string();
    if text == "-0" { "0".to_string() } else { text }
}

/// 截断到列宽（`VARCHAR(500)`）：超长保留**尾部**（结果在最后，比开头更有用）。
fn truncate(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let skip = count.saturating_sub(max.saturating_sub(1));
    let keep: String = text.chars().skip(skip).collect();
    format!("…{keep}")
}

/// 工号（找不到就回 id 前 8 位，报文里不至于空白）。
fn emp_no_of(employees: &[EmployeeHead], employee_id: Uuid) -> String {
    employees
        .iter()
        .find(|head| head.id == employee_id)
        .map(|head| head.emp_no.clone())
        .unwrap_or_else(|| employee_id.to_string().chars().take(8).collect())
}

// ==================== 读库 ====================

/// 当期在职员工（人口口径见模块文档）。
pub async fn load_employees(
    ex: &impl SqlExecutor,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<Vec<EmployeeHead>> {
    // SCOPE-EXEMPT: 工资试算要算全公司（数据权限在接口层的薪资单列表，不在这里）
    let rows = ex
        .query_sql(
            "SELECT e.id, e.emp_no, e.name, e.dept_id, COALESCE(d.dept_name, '') AS dept_name, \
                    e.user_id, e.position_code, e.job_level \
               FROM biz_employee e \
               LEFT JOIN sys_dept d ON d.id = e.dept_id AND d.deleted_at IS NULL \
              WHERE e.deleted_at IS NULL \
                AND e.org_id = ?::uuid \
                AND e.hire_date <= ?::text::date \
                AND (e.leave_date IS NULL OR e.leave_date >= ?::text::date) \
              ORDER BY e.emp_no ASC"
                .to_string(),
            vec![
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(period_end.to_string()),
                Value::Text(period_start.to_string()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let dept_name: String = row.get_typed("dept_name")?;
        out.push(EmployeeHead {
            id: row.get_typed("id")?,
            emp_no: row.get_typed("emp_no")?,
            name: row.get_typed("name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: Some(dept_name).filter(|text| !text.trim().is_empty()),
            user_id: row.get_typed("user_id")?,
            position_code: row.get_typed("position_code")?,
            job_level: row.get_typed("job_level")?,
        });
    }
    Ok(out)
}

/// 当期生效的薪资标准（按 `effective_from DESC` 取，交 [`assemble`] 检出重叠）。
pub async fn load_standards(
    ex: &impl SqlExecutor,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<Vec<StandardRow>> {
    // SCOPE-EXEMPT: 工资试算的取数（只读、按期间收敛）
    let rows = ex
        .query_sql(
            "SELECT employee_id, item_code, amount, effective_from \
               FROM biz_employee_salary \
              WHERE deleted_at IS NULL \
                AND org_id = ?::uuid \
                AND effective_from <= ?::text::date \
                AND (effective_to IS NULL OR effective_to >= ?::text::date) \
              ORDER BY employee_id, item_code, effective_from DESC"
                .to_string(),
            vec![
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(period_end.to_string()),
                Value::Text(period_start.to_string()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(StandardRow {
            employee_id: row.get_typed("employee_id")?,
            item_code: row.get_typed("item_code")?,
            amount: row.get_typed("amount")?,
            effective_from: row.get_typed("effective_from")?,
        });
    }
    Ok(out)
}

/// 当期考勤：`employee_id → (记录 id, 11 个数值列)`。
pub async fn load_attendance(
    ex: &impl SqlExecutor,
    period_id: Uuid,
) -> DbResult<BTreeMap<Uuid, (Uuid, AttendanceNumbers)>> {
    // SCOPE-EXEMPT: 工资试算的取数（按考勤期间收敛，数据权限在考勤台账接口）
    let rows = ex
        .query_sql(
            "SELECT id, employee_id, should_days, actual_days, leave_paid_days, leave_unpaid_days, \
                    sick_days, absent_days, ot_hours_workday, ot_hours_weekend, ot_hours_holiday, \
                    late_count, early_leave_count \
               FROM biz_attendance_record \
              WHERE period_id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await?;
    let mut out: BTreeMap<Uuid, (Uuid, AttendanceNumbers)> = BTreeMap::new();
    for row in &rows {
        let employee_id: Uuid = row.get_typed("employee_id")?;
        let numbers = AttendanceNumbers {
            should_days: row.get_typed("should_days")?,
            actual_days: row.get_typed("actual_days")?,
            leave_paid_days: row.get_typed("leave_paid_days")?,
            leave_unpaid_days: row.get_typed("leave_unpaid_days")?,
            sick_days: row.get_typed("sick_days")?,
            absent_days: row.get_typed("absent_days")?,
            ot_hours_workday: row.get_typed("ot_hours_workday")?,
            ot_hours_weekend: row.get_typed("ot_hours_weekend")?,
            ot_hours_holiday: row.get_typed("ot_hours_holiday")?,
            // 这两列是 `INT`（次数），不是 `NUMERIC`：按 `Decimal` 读会被驱动判
            // 「type mismatch: expected Decimal, got I64」
            late_count: Decimal::from(row.get_typed::<i32>("late_count")?),
            early_leave_count: Decimal::from(row.get_typed::<i32>("early_leave_count")?),
        };
        out.insert(employee_id, (row.get_typed("id")?, numbers));
    }
    Ok(out)
}

// ==================== 写库 ====================

/// 同期间已有薪资单 → 409（`uk_biz_salary_sheet_period` 的人话版，设计 §4.8.3）。
///
/// HR-40 起：**已红冲（`REVERSED`）的单不占期间位** —— 红冲的目的就是「这张别要了，
/// 重算一张」，所以唯一键与这道前置检查都排除 `REVERSED`（迁移 `V0397`）。文案把这层
/// 出口写出来，用户撞 409 时知道下一步是「红冲」而不是「找研发删库」。
pub fn duplicate_sheet_error(period_code: &str, sheet_no: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::HR_SALARY_RULE_BLOCKED,
        format!(
            "{period_code} 的薪资单已存在（{sheet_no}）：同一期间只能有一张有效单（库索引 \
             `uk_biz_salary_sheet_period`）。要重算请打开那张单重跑试算；算错了要重发，\
             请先「红冲」那张单（红冲后同期间可以重新新开一张），或改用补差单（设计 §4.8.2）"
        ),
    )
}

/// 当前期间是否已有薪资单（返回已有单号）。
///
/// `status <> 'REVERSED'` 是 HR-40 加的：红冲过的单是审计凭据（保留红字凭证与原因），
/// 但**不再占用期间位** —— 否则「红冲后重算」永远撞唯一键，客户只能找研发改库。
pub async fn existing_sheet_no(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 唯一性检查（按 org + 期间）
    let row = ex
        .query_first_sql(
            "SELECT sheet_no FROM biz_salary_sheet \
              WHERE org_id = ?::uuid AND period_code = ? AND deleted_at IS NULL \
                AND status <> 'REVERSED'"
                .to_string(),
            vec![Value::from(&SENTINEL_ORG_ID), Value::Text(period_code.to_string())],
        )
        .await?;
    row.as_ref().map(|row| row.get_typed("sheet_no")).transpose()
}

/// 取一个薪资单号（`SAL-yyyyMM-NNNNNN`，与建单**同一事务**）。
///
/// 周期键取**薪资期间**（不是建单当天）：补做 10 月的单号必须是 `SAL-202610-*`。
/// 回拨保护沿用 `serial_model` / `voucher::bump_seq` 的口径：`period_key` 比当前小
/// （补做往期）时**不重置**计数，只顺延 —— 宁可号不连续，绝不重号。
pub async fn next_sheet_no(
    ex: &impl SqlExecutor,
    period_code: &str,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<String> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典。
    let rule = ex
        .query_first_sql(
            "SELECT prefix, separator, pad_width, date_pattern FROM biz_seq \
              WHERE seq_type = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(SHEET_SEQ_TYPE.to_string())],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(format!(
                "取号失败：biz_seq(seq_type='{SHEET_SEQ_TYPE}') 行不存在\
                 （应由 V0386__hr_seq_dict 种入，请检查迁移）"
            ))
        })?;
    let prefix: String = rule.get_typed("prefix")?;
    let separator: String = rule.get_typed("separator")?;
    let pad_width: i32 = i32::from(rule.get_typed::<i16>("pad_width")?);
    let date_pattern: String = rule.get_typed("date_pattern")?;
    let period_key = period_key_for(&date_pattern, period_start, period_end);

    // SCOPE-EXEMPT: biz_seq 写入（取号）。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(SHEET_SEQ_TYPE.to_string()),
            ],
        )
        .await?;
    let no: i64 = match rows.first() {
        Some(row) => row.get_typed("current_no")?,
        None => {
            return Err(DbError::InvalidValue(format!(
                "取号失败：biz_seq(seq_type='{SHEET_SEQ_TYPE}') 行在更新时消失了"
            )));
        }
    };
    let _ = period_code;
    Ok(format_sheet_no(&prefix, &separator, &period_key, pad_width, no))
}

/// 单头插入所需的一切（[`calc_with_items`] 在事务里拼）。
#[derive(Debug, Clone)]
pub struct SheetDraft {
    /// 单头 id。
    pub id: Uuid,
    /// 单号。
    pub sheet_no: String,
    /// 薪资期间 `yyyy-MM`。
    pub period_code: String,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 数据来源考勤期间。
    pub attendance_period_id: Option<Uuid>,
    /// 合计。
    pub totals: CalcTotals,
    /// 备注。
    pub remark: Option<String>,
}

/// 插单头（状态 `DRAFT`，随后由状态机推到 `CALCULATED`）。
pub async fn insert_sheet(ex: &impl SqlExecutor, draft: &SheetDraft, operator: Option<Uuid>) -> DbResult<()> {
    // SCOPE-EXEMPT: 建单写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        "INSERT INTO biz_salary_sheet \
           (id, org_id, sheet_no, period_code, period_start, period_end, status, \
            attendance_period_id, employee_count, earning_total, pre_deduct_total, gross_total, \
            withhold_total, net_total, si_company_total, hf_company_total, remark, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?, ?, ?::text::date, ?::text::date, ?, ?::uuid, \
                 ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 NULLIF(?::text, ''), ?::uuid, ?::uuid)"
            .to_string(),
        vec![
            Value::from(&draft.id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text(draft.sheet_no.clone()),
            Value::Text(draft.period_code.clone()),
            Value::Text(draft.period_start.to_string()),
            Value::Text(draft.period_end.to_string()),
            Value::Text(STATUS_DRAFT.to_string()),
            opt_uuid(draft.attendance_period_id),
            Value::from(draft.totals.employee_count),
            num(draft.totals.earning_total),
            num(draft.totals.pre_deduct_total),
            num(draft.totals.gross_total),
            num(draft.totals.withhold_total),
            num(draft.totals.net_total),
            num(draft.totals.si_company_total),
            num(draft.totals.hf_company_total),
            Value::Text(draft.remark.clone().unwrap_or_default()),
            opt_uuid(operator),
            opt_uuid(operator),
        ],
    )
    .await?;
    Ok(())
}

/// 试算完成 → 单头状态 `DRAFT → CALCULATED`（CAS 回写 + 计算时间/人）。
///
/// 受影响行数 `0` = 状态不是 `DRAFT`（被别人改过）→ 调用方报 409。
pub async fn mark_calculated(ex: &impl SqlExecutor, sheet_id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单头状态回写（CAS 在 SQL 里）。
    ex.exec_sql(
        "UPDATE biz_salary_sheet \
            SET status = ?, calculated_at = now(), calculated_by = ?::uuid, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(STATUS_CALCULATED.to_string()),
            opt_uuid(operator),
            opt_uuid(operator),
            Value::from(&sheet_id),
            Value::Text(STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

/// 批量插明细 + 明细项（**一个事务内**，由调用方给执行器）。
///
/// `items` 用来把 `item_code` 回填成 `item_id`（`detail_item.item_id`，
/// 薪资项被删后靠 `expr_snapshot` + `ON DELETE SET NULL` 留痕）。
pub async fn insert_details(ex: &impl SqlExecutor, sheet_id: Uuid, slips: &[Payslip], engine: &Engine) -> DbResult<()> {
    // 明细行 id 先分配好：明细项要在**同一批**里写 `detail_id`，而批量插入不回传每行 id
    // （省掉一轮 `RETURNING` 往返，也省掉「按人再查一次」的 N+1）
    let detail_ids: Vec<Uuid> = slips.iter().map(|_| Uuid::now_v7()).collect();

    for (chunk, chunk_ids) in slips.chunks(DETAIL_CHUNK).zip(detail_ids.chunks(DETAIL_CHUNK)) {
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * DETAIL_COLUMNS);
        let mut tuples: Vec<String> = Vec::with_capacity(chunk.len());
        for (slip, detail_id) in chunk.iter().zip(chunk_ids.iter()) {
            tuples.push(DETAIL_TUPLE.to_string());
            params.extend([
                Value::from(detail_id),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&sheet_id),
                Value::from(&slip.head.id),
                Value::Text(slip.head.emp_no.clone()),
                Value::Text(slip.head.name.clone()),
                opt_uuid(slip.head.dept_id),
                opt_text(slip.head.dept_name.as_deref()),
                opt_uuid(slip.head.user_id),
                opt_text(slip.head.position_code.as_deref()),
                opt_text(slip.head.job_level.as_deref()),
                num(slip.attendance.should_days),
                num(slip.attendance.actual_days),
                num(slip.attendance.leave_unpaid_days),
                num(slip.attendance.ot_hours_total()),
                num(slip.earning_amount),
                num(slip.pre_deduct_amount),
                num(slip.gross_amount),
                num(slip.social_personal),
                num(slip.fund_personal),
                num(slip.iit_amount),
                num(slip.other_withhold),
                num(slip.withhold_amount),
                num(slip.net_amount),
                num(slip.si_company),
                num(slip.hf_company),
                opt_text(slip.remark.as_deref()),
            ]);
        }
        // SCOPE-EXEMPT: 明细写入（随单头级联）。
        ex.exec_sql(format!("{DETAIL_SQL} VALUES {}", tuples.join(", ")), params)
            .await?;
    }

    // 明细项：逐人 N 行（`SUMMARY` 不落）
    for (slip, detail_id) in slips.iter().zip(detail_ids.iter()) {
        for chunk in slip.items.chunks(ITEM_CHUNK) {
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * ITEM_COLUMNS);
            let mut tuples: Vec<String> = Vec::with_capacity(chunk.len());
            for item in chunk {
                tuples.push(ITEM_TUPLE.to_string());
                let item_id = engine.item(&item.item_code).and_then(|meta| meta.id);
                params.extend([
                    Value::from(&Uuid::now_v7()),
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(&sheet_id),
                    Value::from(detail_id),
                    opt_uuid(item_id),
                    Value::Text(item.item_code.clone()),
                    Value::Text(item.item_name.clone()),
                    Value::Text(item.item_type.clone()),
                    Value::Text(item.calc_type.clone()),
                    num(item.amount),
                    opt_text(item.expr_snapshot.as_deref()),
                    Value::Text(item.calc_note.clone()),
                    Value::from(item.line_no),
                ]);
            }
            // SCOPE-EXEMPT: 明细项写入（随单头级联）。
            ex.exec_sql(format!("{ITEM_SQL} VALUES {}", tuples.join(", ")), params)
                .await?;
        }
    }
    Ok(())
}

/// 明细行插入的列数（与 [`DETAIL_TUPLE`] 逐个对齐；单测对拍）。
const DETAIL_COLUMNS: usize = 27;

/// 明细行占位符（27 列）。
const DETAIL_TUPLE: &str = "(?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, ?, ?::uuid, NULLIF(?::text, ''), \
     ?::uuid, NULLIF(?::text, ''), NULLIF(?::text, ''), ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
     ?::text::numeric, ?::text::numeric, ?::text::numeric, NULLIF(?::text, ''))";

/// 明细行 INSERT 头。
const DETAIL_SQL: &str = "INSERT INTO biz_salary_detail \
     (id, org_id, sheet_id, employee_id, employee_no, employee_name, dept_id, dept_name, user_id, \
      position_code, job_level, should_days, actual_days, leave_unpaid_days, ot_hours, \
      earning_amount, pre_deduct_amount, gross_amount, social_personal, fund_personal, iit_amount, \
      other_withhold, withhold_amount, net_amount, si_company, hf_company, remark)";

/// 明细项插入的列数（与 [`ITEM_TUPLE`] 逐个对齐；单测对拍）。
const ITEM_COLUMNS: usize = 13;

/// 明细项占位符（13 列）。
const ITEM_TUPLE: &str = "(?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?, ?::text::numeric, \
     NULLIF(?::text, ''), ?, ?)";

/// 明细项 INSERT 头。
const ITEM_SQL: &str = "INSERT INTO biz_salary_detail_item \
     (id, org_id, sheet_id, detail_id, item_id, item_code, item_name, item_type, calc_type, amount, \
      expr_snapshot, calc_note, line_no)";

// ==================== 编排 ====================

/// 试算 / 建单的**唯一**入口（试算与正式计算同源）。
///
/// # 参数
///
/// * `operator`：操作人（落 `create_by` / `calculated_by` 与状态日志）；
/// * `req`：期间 / 考勤期间 / `dry_run` / 人工录入 / 备注；
/// * `items`：薪资项**全部**行（停用的也传：`salary_vars` 要区分「停用」与「不存在」），
///   顺序即 `sort_order`。生产入口 [`calc`] 从库里读，本函数供「上游已加载项集」与单测复用。
///
/// # 事务边界
///
/// 一个事务包住「读标准/考勤/员工 + 算 + 写单头/明细/明细项 + 状态流转」：
///
/// * `dry_run = true` → 算完 `rollback`（库里零行，号段也不推进）；
/// * `dry_run = false` → `commit`；任一步失败 → `rollback`，**不存在**「单头建了明细没写」的半张单
///   （`AGENTS.md` 约定 11）。
pub async fn calc_with_items(
    operator: Operator,
    req: &CalcRequest,
    items: &[SalaryItemRow],
) -> std::result::Result<CalcOutcome, ApiError> {
    let engine = Engine::of_rows(items).map_err(|issues| plan_error(&issues))?;
    if engine.is_empty() {
        return Err(issues_error(&[CalcIssue::NoActiveItems]));
    }

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启薪资试算事务"))?;

    let outcome = match run_in_tx(&tx, operator, req, &engine).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    if req.dry_run {
        // 试算：库里的写入一条都没有（号段也不推进），事务必须回滚
        if let Err(error) = tx.rollback().await {
            return Err(utils::db_error(error, "回滚薪资试算事务"));
        }
        return Ok(outcome);
    }

    match tx.commit().await {
        Ok(()) => Ok(outcome),
        Err(error) => Err(utils::db_error(error, "提交薪资单事务")),
    }
}

/// 生产入口：从库里读薪资项，再走 [`calc_with_items`]。
///
/// 为什么项集单独读一次（不在试算事务里）：`Engine` 要在开事务**之前**建好（公式配置坏了
/// 就别开事务白跑一趟）；薪资项是配置表，改它要走配置接口（并发改配置的概率远低于改考勤）。
pub async fn calc(operator: Operator, req: &CalcRequest) -> std::result::Result<CalcOutcome, ApiError> {
    let items = item_model::list_all(db()).await?;
    calc_with_items(operator, req, &items).await
}

/// 事务内的主体：解析期间 → 取数 → 算 → （非试算时）落库。
async fn run_in_tx(
    tx: &impl SqlExecutor,
    operator: Operator,
    req: &CalcRequest,
    engine: &Engine,
) -> std::result::Result<CalcOutcome, ApiError> {
    let (period_start, period_end, attendance_period_id) = resolve_period(tx, req).await?;

    // 建单前的快失败：同期间已有单直接 409（不让用户等算完 200 人才知道）
    if !req.dry_run
        && let Some(sheet_no) = existing_sheet_no(tx, &req.period_code).await?
    {
        return Err(duplicate_sheet_error(&req.period_code, &sheet_no));
    }

    let employees = load_employees(tx, period_start, period_end).await?;
    let standards = load_standards(tx, period_start, period_end).await?;
    let attendance = match attendance_period_id {
        Some(period_id) => load_attendance(tx, period_id).await?,
        None => BTreeMap::new(),
    };

    engine
        .validate_manual(&req.manual, &employees)
        .map_err(|issues| issues_error(&issues))?;
    let inputs = assemble(&employees, &standards, &attendance, &req.manual).map_err(|issues| issues_error(&issues))?;
    let slips = engine.compute_all(&inputs).map_err(|issues| issues_error(&issues))?;
    let totals = CalcTotals::of(&slips);
    let missing_attendance: Vec<String> = inputs
        .iter()
        .filter(|input| input.attendance.is_none())
        .map(|input| format!("{} {}", input.head.emp_no, input.head.name))
        .collect();
    let item_count = engine.detail_item_count();

    let mut outcome = CalcOutcome {
        period_code: req.period_code.clone(),
        period_start,
        period_end,
        attendance_period_id,
        dry_run: req.dry_run,
        totals,
        slips,
        sheet: None,
        missing_attendance,
        item_count,
    };

    if req.dry_run {
        return Ok(outcome);
    }

    // ---- 落库：单头 → 建单日志 → DRAFT→CALCULATED → 明细 → 明细项 ----
    let sheet_id = Uuid::now_v7();
    let sheet_no = next_sheet_no(tx, &req.period_code, period_start, period_end).await?;
    let draft = SheetDraft {
        id: sheet_id,
        sheet_no: sheet_no.clone(),
        period_code: req.period_code.clone(),
        period_start,
        period_end,
        attendance_period_id,
        totals,
        remark: req.remark.clone(),
    };
    insert_sheet(tx, &draft, operator.id)
        .await
        .map_err(|error| translate_write_error(error, &req.period_code, &sheet_no, "建薪资单"))?;

    // 状态日志用 clone：`log_created` / `transition` 都要「整个 operator」（含用户名），
    // 而 `mark_calculated` 只要 id —— 三次都用得上，所以全程不 move 掉它。
    let actor = Operator::named(operator.id, operator.name.clone().unwrap_or_default());
    state_machine::log_created(
        tx,
        SALARY_SHEET_ENTITY,
        sheet_id,
        &sheet_no,
        STATUS_DRAFT,
        actor.clone(),
        Some("薪资试算建单"),
    )
    .await?;
    state_machine::transition(
        tx,
        SALARY_SHEET_ENTITY,
        sheet_id,
        &sheet_no,
        &state_machine::SALARY_SHEET_EDGES,
        STATUS_DRAFT,
        STATUS_CALCULATED,
        actor,
        Some("试算完成（逐人逐项落库）"),
        None,
    )
    .await?;
    let updated = mark_calculated(tx, sheet_id, operator.id)
        .await
        .map_err(|error| utils::db_error(error, "回写薪资单状态"))?;
    if updated == 0 {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!("薪资单 {sheet_no} 的状态已被他人改动（不是 DRAFT），本次试算结果未落库，请刷新后重试"),
        ));
    }
    insert_details(tx, sheet_id, &outcome.slips, engine)
        .await
        .map_err(|error| translate_write_error(error, &req.period_code, &sheet_no, "写薪资明细"))?;

    outcome.sheet = Some(SheetRef {
        id: sheet_id,
        sheet_no,
        status: STATUS_CALCULATED.to_string(),
    });
    Ok(outcome)
}

/// 解析薪资期间与数据来源考勤期间。
///
/// * 给了 `attendance_period_id`：用它（并校验 `period_code` 与它一致 —— 两处不一致时
///   「按 11 月的工资算 10 月的考勤」是最难查的一类错）；
/// * 没给：按 `period_code` 找同期考勤期间，找不到就当本期没有考勤（考勤变量按 0 计）。
async fn resolve_period(
    ex: &impl SqlExecutor,
    req: &CalcRequest,
) -> std::result::Result<(NaiveDate, NaiveDate, Option<Uuid>), ApiError> {
    if let Some(period_id) = req.attendance_period_id {
        let row = period_model::find_in(ex, period_id).await?.ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("考勤期间 {period_id} 不存在或已删除：请重新选择数据来源期间"),
            )
        })?;
        if row.period_code != req.period_code.trim() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "薪资期间 `{}` 与考勤期间 `{}` 不一致：请让两者指向同一个月\
                     （按 11 月的工资算 10 月的考勤是最难查的一类错）",
                    req.period_code, row.period_code
                ),
            ));
        }
        return Ok((row.period_start, row.period_end, Some(row.id)));
    }

    let (start, end) = period_range(&req.period_code)?;
    // SCOPE-EXEMPT: 期间头只读（按 org + 期间码定位）。
    let row = ex
        .query_first_sql(
            "SELECT id, period_code, period_start, period_end FROM biz_attendance_period \
              WHERE org_id = ?::uuid AND period_code = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(req.period_code.trim().to_string()),
            ],
        )
        .await?;
    match row {
        Some(row) => {
            let period_code: String = row.get_typed("period_code")?;
            if period_code != req.period_code.trim() {
                return Ok((start, end, None));
            }
            Ok((
                row.get_typed("period_start")?,
                row.get_typed("period_end")?,
                Some(row.get_typed("id")?),
            ))
        }
        None => Ok((start, end, None)),
    }
}

/// 写库错误 → 用户话（撞期间唯一键 / 撞 CHECK 各说一句）。
fn translate_write_error(error: DbError, period_code: &str, sheet_no: &str, ctx: &str) -> ApiError {
    match &error {
        DbError::UniqueViolation { constraint, .. } => {
            if constraint.as_deref() == Some("uk_biz_salary_sheet_period") {
                return duplicate_sheet_error(period_code, sheet_no);
            }
            ApiError::new(
                409,
                error_code::HR_SALARY_RULE_BLOCKED,
                format!(
                    "{ctx}撞了唯一约束（{}）：{sheet_no}",
                    constraint.as_deref().unwrap_or("?")
                ),
            )
        }
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "{ctx}被库约束拒绝（{}）：薪资单 {sheet_no} 的合计不满足恒等式，这是引擎 bug 或绕过引擎的写路径，请报给开发",
                constraint.as_deref().unwrap_or("?")
            ),
        ),
        _ => utils::db_error(error, ctx),
    }
}

/// `Option<Uuid>` → 绑定值（`?::uuid` 位；`None` 用 `Value::Null`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::from(&id),
        None => Value::Null,
    }
}

/// 可空文本（SQL 里包 `NULLIF(?::text, '')`）。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// `NUMERIC` 参数：走 `?::text::numeric` + 文本形态（驱动的 `Decimal` 绑定会丢精度）。
fn num(value: Decimal) -> Value {
    Value::Text(value.to_string())
}

/// 取 `biz_seq` 当前号（真库用例对拍「试算不推进号段」）。
pub async fn seq_current_no(ex: &impl SqlExecutor) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: biz_seq 只读。
    let row = ex
        .query_first_sql(
            "SELECT current_no FROM biz_seq WHERE seq_type = ? AND deleted_at IS NULL".to_string(),
            vec![Value::Text(SHEET_SEQ_TYPE.to_string())],
        )
        .await?;
    row.as_ref().map(|row| row.get_typed("current_no")).transpose()
}

// ==================== 单测（纯函数，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    // 测试里的种子表要写 `FORMULA` 字面常量（生产代码只在 match 分支里比字符串）
    use crate::business::hr::salary_item_dto::CALC_TYPE_FORMULA;

    /// 设计 §4.4.3 的 17 个种子项（15 计算项 + 2 汇总项）：编码 / 名称 / 类型 / 算法 / 公式。
    ///
    /// 逐条照抄设计表 —— 这份表是**引擎的输入契约**，种子里改一项就要在
    /// `V0387` 迁移和 HR-35 的验收用例里同步，两边漂了这里先红。
    fn seed_rows() -> Vec<(
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    )> {
        vec![
            ("BASE", "基本工资", ITEM_TYPE_EARNING, CALC_TYPE_FIXED, None),
            ("POST", "岗位津贴", ITEM_TYPE_EARNING, CALC_TYPE_FIXED, None),
            ("MEAL", "餐补", ITEM_TYPE_EARNING, CALC_TYPE_FIXED, None),
            ("PERF", "绩效奖金", ITEM_TYPE_EARNING, CALC_TYPE_INPUT, None),
            (
                "OT_WD",
                "平时加班费",
                ITEM_TYPE_EARNING,
                CALC_TYPE_FORMULA,
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)"),
            ),
            (
                "OT_WE",
                "休息日加班费",
                ITEM_TYPE_EARNING,
                CALC_TYPE_FORMULA,
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)"),
            ),
            (
                "OT_HO",
                "节假日加班费",
                ITEM_TYPE_EARNING,
                CALC_TYPE_FORMULA,
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)"),
            ),
            (
                "ATT_BONUS",
                "全勤奖",
                ITEM_TYPE_EARNING,
                CALC_TYPE_FORMULA,
                Some("IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)"),
            ),
            (
                "ATT_DEDUCT",
                "考勤扣款",
                ITEM_TYPE_PRE_DEDUCT,
                CALC_TYPE_FORMULA,
                Some("ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)"),
            ),
            ("SI_PERSONAL", "社保(个人)", ITEM_TYPE_WITHHOLD, CALC_TYPE_INPUT, None),
            ("HF_PERSONAL", "公积金(个人)", ITEM_TYPE_WITHHOLD, CALC_TYPE_INPUT, None),
            ("IIT", "个人所得税", ITEM_TYPE_WITHHOLD, CALC_TYPE_INPUT, None),
            ("OTHER_WITHHOLD", "其他代扣", ITEM_TYPE_WITHHOLD, CALC_TYPE_INPUT, None),
            ("SI_COMPANY", "单位社保", ITEM_TYPE_EMPLOYER, CALC_TYPE_INPUT, None),
            ("HF_COMPANY", "单位公积金", ITEM_TYPE_EMPLOYER, CALC_TYPE_INPUT, None),
            (
                "GROSS",
                "应发合计",
                ITEM_TYPE_SUMMARY,
                CALC_TYPE_FORMULA,
                Some("{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}"),
            ),
            (
                "NET",
                "实发合计",
                ITEM_TYPE_SUMMARY,
                CALC_TYPE_FORMULA,
                Some("{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}"),
            ),
        ]
    }

    /// 用「元组表」建引擎（与 [`Engine::of_rows`] 同一条路径，只是不入库）。
    fn engine_of(
        rows: &[(
            &'static str,
            &'static str,
            &'static str,
            &'static str,
            Option<&'static str>,
        )],
    ) -> Engine {
        let specs: Vec<ItemSpec> = rows
            .iter()
            .map(|(code, name, _item_type, calc_type, formula)| ItemSpec::new(*code, *name, *calc_type, *formula))
            .collect();
        let plan = salary_vars::build_plan(&specs).expect("种子项必须建得出计划");
        let metas: Vec<ItemMeta> = rows
            .iter()
            .map(|(code, name, item_type, calc_type, formula)| {
                ItemMeta::new(*code, *name, *item_type, *calc_type, *formula)
            })
            .collect();
        Engine::new(plan, metas)
    }

    fn seed_engine() -> Engine {
        engine_of(&seed_rows())
    }

    fn head(emp_no: &str, name: &str) -> EmployeeHead {
        EmployeeHead {
            id: Uuid::now_v7(),
            emp_no: emp_no.to_string(),
            name: name.to_string(),
            dept_id: None,
            dept_name: Some("生产部".to_string()),
            user_id: None,
            position_code: Some("WORKER".to_string()),
            job_level: Some("P3".to_string()),
        }
    }

    fn amount(text: &str) -> Decimal {
        text.parse().expect("测试里的金额字面量必须能解析")
    }

    /// 设计 §4.4.4 的张三：BASE 8000 / POST 500 / MEAL 300 / PERF 1200；
    /// 考勤 should 22 / actual 20 / 无薪假 2 / 平时加班 10h；社保个人 840、公积金个人 960，
    /// 个税 0、其他 0；单位社保 1680、单位公积金 960。
    fn zhangsan_input() -> PayInput {
        let mut input = PayInput::new(head("EMP-0001", "张三"));
        for (code, value) in [("BASE", "8000"), ("POST", "500"), ("MEAL", "300")] {
            input.standard.insert(code.to_string(), amount(value));
        }
        for (code, value) in [
            ("PERF", "1200"),
            ("SI_PERSONAL", "840"),
            ("HF_PERSONAL", "960"),
            ("IIT", "0"),
            ("OTHER_WITHHOLD", "0"),
            ("SI_COMPANY", "1680"),
            ("HF_COMPANY", "960"),
        ] {
            input.manual.insert(code.to_string(), amount(value));
        }
        input.attendance = Some(AttendanceNumbers {
            should_days: Decimal::from(22),
            actual_days: Decimal::from(20),
            leave_unpaid_days: Decimal::from(2),
            ot_hours_workday: Decimal::from(10),
            ..AttendanceNumbers::default()
        });
        input.attendance_record_id = Some(Uuid::now_v7());
        input
    }

    // ---- 验收项：实测算例 + 恒等式 ----

    /// 设计 §4.4.4 的四个实测数字 + 逐人恒等式全绿。
    #[test]
    fn design_example_matches_section_4_4_4() {
        let engine = seed_engine();
        let slip = engine.compute(&zhangsan_input()).expect("张三必须算得出来");

        let item = |code: &str| {
            slip.items
                .iter()
                .find(|item| item.item_code == code)
                .unwrap_or_else(|| panic!("{code} 必须在明细项里"))
                .amount
        };
        assert_eq!(item("OT_WD"), amount("689.66"), "平时加班费");
        assert_eq!(item("ATT_DEDUCT"), amount("735.63"), "考勤扣款（中间不许提前四舍五入）");
        assert_eq!(item("ATT_BONUS"), Decimal::ZERO, "有 2 天事假 → 无全勤奖");

        assert_eq!(slip.earning_amount, amount("10689.66"), "Σ EARNING");
        assert_eq!(slip.pre_deduct_amount, amount("735.63"), "Σ PRE_DEDUCT");
        assert_eq!(slip.gross_amount, amount("9954.03"), "应发");
        assert_eq!(slip.withhold_amount, amount("1800.00"), "代扣合计");
        assert_eq!(slip.net_amount, amount("8154.03"), "实发");
        assert_eq!(slip.si_company, amount("1680"));
        assert_eq!(slip.hf_company, amount("960"));
        assert_eq!(slip.accrual_amount(), amount("12594.03"), "计提总额");

        // 恒等式（验收第 1 条）
        assert!(slip.check().is_ok(), "恒等式：{:?}", slip.check().err());
        assert_eq!(
            slip.gross_amount - slip.withhold_amount,
            slip.net_amount,
            "应发 − 代扣 = 实发（逐人）"
        );

        // `SUMMARY` 不落明细项：15 个计算项
        assert_eq!(slip.items.len(), 15);
        assert!(slip.items.iter().all(|item| item.item_type != ITEM_TYPE_SUMMARY));
        // 行号连续（拓扑序）
        let line_nos: Vec<i32> = slip.items.iter().map(|item| item.line_no).collect();
        assert_eq!(line_nos, (1..=15).collect::<Vec<i32>>());
        // 公式快照 + 代入说明都在
        let ot = slip.items.iter().find(|item| item.item_code == "OT_WD").expect("OT_WD");
        assert!(ot.expr_snapshot.as_deref().unwrap_or_default().contains("ATT_OT_WD"));
        assert!(
            ot.calc_note.contains("8000") && ot.calc_note.contains("689.66"),
            "calc_note 要能对拍 Excel：{}",
            ot.calc_note
        );
        // 固定项 / 人工项的说明
        let base = slip.items.iter().find(|item| item.item_code == "BASE").expect("BASE");
        assert!(base.calc_note.contains("薪资标准"), "{}", base.calc_note);
        assert_eq!(base.expr_snapshot, None, "非公式项没有公式快照");
    }

    /// 缺标准 / 缺人工录入 → 按 0 算，但说明里写清「未配 / 未录入」（不静默）。
    #[test]
    fn missing_standard_and_manual_default_to_zero_with_note() {
        let engine = seed_engine();
        let mut input = PayInput::new(head("EMP-0002", "李四"));
        input.attendance = Some(AttendanceNumbers {
            should_days: Decimal::from(22),
            actual_days: Decimal::from(22),
            ..AttendanceNumbers::default()
        });
        let slip = engine.compute(&input).expect("全 0 也是合法的一张工资条");
        assert_eq!(slip.earning_amount, amount("200"), "只有全勤奖 200（无旷工、无无薪假）");
        assert_eq!(slip.gross_amount, amount("200"));
        assert_eq!(slip.net_amount, amount("200"));
        let base = slip.items.iter().find(|item| item.item_code == "BASE").expect("BASE");
        assert_eq!(base.amount, Decimal::ZERO);
        assert!(base.calc_note.contains("未配薪资标准"), "{}", base.calc_note);
        let perf = slip.items.iter().find(|item| item.item_code == "PERF").expect("PERF");
        assert!(perf.calc_note.contains("未录入"), "{}", perf.calc_note);
    }

    /// 没有考勤记录 → 考勤变量全 0 + 明细备注写明（HR 能看见「这个人是按 0 算的」）。
    #[test]
    fn missing_attendance_is_noted_on_the_detail() {
        let engine = seed_engine();
        let mut input = PayInput::new(head("EMP-0003", "王五"));
        input.standard.insert("BASE".to_string(), amount("8000"));
        let slip = engine.compute(&input).expect("没有考勤也要能算");
        assert_eq!(slip.attendance, AttendanceNumbers::default());
        assert_eq!(slip.attendance_record_id, None);
        assert!(
            slip.remark.as_deref().unwrap_or_default().contains("无考勤记录"),
            "{:?}",
            slip.remark
        );
    }

    // ---- 验收项：恒等式与交叉校验 ----

    /// `GROSS` 公式漏了考勤扣款 → 交叉校验报「应发合计不一致」，指名人和两个数。
    #[test]
    fn gross_formula_drift_is_caught_by_cross_check() {
        let mut rows = seed_rows();
        let index = rows.iter().position(|row| row.0 == "GROSS").expect("GROSS");
        rows[index].4 = Some("({BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS})");
        let engine = engine_of(&rows);

        let issues = engine.compute(&zhangsan_input()).expect_err("应发对不上必须报");
        let identity = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::Identity { .. }))
            .expect("必须报 Identity");
        let message = identity.message();
        assert!(message.contains("EMP-0001"), "{message}");
        assert!(message.contains("9954.03"), "聚合侧：{message}");
        assert!(message.contains("10689.66"), "公式侧：{message}");
        assert!(message.contains("应发合计"), "{message}");
    }

    /// `NET` 公式漏了个税 → 同样被交叉校验抓住（不是等库层 CHECK 报运维语言）。
    #[test]
    fn net_formula_drift_is_caught_by_cross_check() {
        let mut rows = seed_rows();
        let index = rows.iter().position(|row| row.0 == "NET").expect("NET");
        rows[index].4 = Some("{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{OTHER_WITHHOLD}");
        let engine = engine_of(&rows);

        let mut input = zhangsan_input();
        input.manual.insert("IIT".to_string(), amount("300"));
        let issues = engine.compute(&input).expect_err("实发对不上必须报");
        let message = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::Identity { .. }))
            .expect("必须报 Identity")
            .message();
        assert!(message.contains("实发合计"), "{message}");
        assert!(message.contains("EMP-0001"), "{message}");
    }

    /// 手改过的工资条（例如绕过引擎直接写库）→ `check_identity` 逐条抓出来。
    #[test]
    fn check_identity_catches_hand_edited_slip() {
        let engine = seed_engine();
        let mut slip = engine.compute(&zhangsan_input()).expect("张三");
        slip.net_amount = amount("8154.04");
        let issues = slip.check().expect_err("改过实发必须报");
        assert!(
            issues.iter().any(|issue| issue.message().contains("实发恒等式")),
            "{issues:?}"
        );

        let mut slip = engine.compute(&zhangsan_input()).expect("张三");
        slip.withhold_amount = amount("1800.01");
        let issues = slip.check().expect_err("改过代扣必须报");
        assert!(
            issues.iter().any(|issue| issue.message().contains("代扣恒等式")),
            "{issues:?}"
        );

        let mut slip = engine.compute(&zhangsan_input()).expect("张三");
        slip.gross_amount = amount("9000");
        let issues = slip.check().expect_err("改过应发必须报");
        assert!(
            issues.iter().any(|issue| issue.message().contains("应发恒等式")),
            "{issues:?}"
        );
    }

    /// 考勤扣款超过收入 → 应发为负：报错并给出处置（设计 §4.8.5 一期不许负应发）。
    #[test]
    fn negative_gross_is_reported_with_actionable_message() {
        let engine = seed_engine();
        let mut input = PayInput::new(head("EMP-0004", "赵六"));
        input.standard.insert("BASE".to_string(), amount("1000"));
        input.attendance = Some(AttendanceNumbers {
            should_days: Decimal::from(22),
            actual_days: Decimal::from(0),
            leave_unpaid_days: Decimal::from(30),
            absent_days: Decimal::from(30),
            ..AttendanceNumbers::default()
        });
        let issues = engine.compute(&input).expect_err("负应发必须报");
        let negative = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::NegativeGross { .. }))
            .expect("必须报 NegativeGross");
        let message = negative.message();
        assert!(message.contains("EMP-0004") && message.contains("赵六"), "{message}");
        assert!(message.contains("负应发"), "{message}");
        assert!(message.contains("明细覆盖"), "要给出处置：{message}");
    }

    /// 代扣超过应发 → 实发为负：报错（库层 CHECK 也会拒，但报文得是人话）。
    #[test]
    fn negative_net_is_reported() {
        let engine = seed_engine();
        let mut input = zhangsan_input();
        input.manual.insert("OTHER_WITHHOLD".to_string(), amount("20000"));
        let issues = engine.compute(&input).expect_err("负实发必须报");
        let message = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::NegativeNet { .. }))
            .expect("必须报 NegativeNet")
            .message();
        assert!(message.contains("实发为负"), "{message}");
        assert!(message.contains("21800") || message.contains("20000"), "{message}");
    }

    // ---- 取数口径 ----

    /// 白名单里每一列都能从 [`AttendanceNumbers`] 读到（加了列忘加映射 → 公式静默算 0）。
    #[test]
    fn every_whitelisted_column_is_readable() {
        let numbers = AttendanceNumbers {
            should_days: amount("22"),
            actual_days: amount("20"),
            leave_paid_days: amount("1"),
            leave_unpaid_days: amount("2"),
            sick_days: amount("3"),
            absent_days: amount("4"),
            ot_hours_workday: amount("5"),
            ot_hours_weekend: amount("6"),
            ot_hours_holiday: amount("7"),
            late_count: amount("8"),
            early_leave_count: amount("9"),
        };
        for (var, column, label) in crate::business::hr::salary_item_formula::ATT_VARS {
            let value = numbers
                .value_of(column)
                .unwrap_or_else(|| panic!("白名单变量 {var}（{label}）读不到列 {column}"));
            assert!(value > Decimal::ZERO, "{var} = {value}");
        }
        assert_eq!(numbers.value_of("no_such_column"), None);
        assert_eq!(numbers.ot_hours_total(), amount("18"));
    }

    /// 代扣 / 单位承担项的编码 → 明细列桶；一套映射两端一致（四桶之和 = 代扣合计）。
    #[test]
    fn bucket_mapping_covers_seed_codes() {
        assert_eq!(withhold_bucket(WITHHOLD_SOCIAL_CODE), WithholdBucket::Social);
        assert_eq!(withhold_bucket(WITHHOLD_FUND_CODE), WithholdBucket::Fund);
        assert_eq!(withhold_bucket(WITHHOLD_IIT_CODE), WithholdBucket::Iit);
        assert_eq!(withhold_bucket("OTHER_WITHHOLD"), WithholdBucket::Other);
        assert_eq!(withhold_bucket("BACK_PAY_DEDUCT"), WithholdBucket::Other);

        // 编码命中优先（一期 17 个种子项）
        assert_eq!(employer_bucket(EMPLOYER_SOCIAL_CODE, None), EmployerBucket::Social);
        assert_eq!(employer_bucket(EMPLOYER_FUND_CODE, None), EmployerBucket::Fund);
        // 编码没命中时看贷方科目（客户自建 / 改名的单位承担项）
        assert_eq!(
            employer_bucket("SI_COMPANY_2", Some(ACCOUNT_SI_COMPANY)),
            EmployerBucket::Social
        );
        assert_eq!(
            employer_bucket("HF_COMPANY_2", Some(ACCOUNT_HF_COMPANY)),
            EmployerBucket::Fund
        );
        // 编码与科目都不中 → 不猜列
        assert_eq!(employer_bucket("UNION_FEE", None), EmployerBucket::Unclassified);
        assert_eq!(
            employer_bucket("UNION_FEE", Some("5602.10")),
            EmployerBucket::Unclassified
        );
    }

    /// 客户自建的单位承担项（编码不是种子项）靠贷方科目归位，照旧进 si_company / hf_company。
    #[test]
    fn employer_items_fall_back_to_credit_account() {
        let mut rows = seed_rows();
        rows.retain(|row| row.0 != "SI_COMPANY" && row.0 != "HF_COMPANY");
        let mut specs: Vec<ItemSpec> = rows
            .iter()
            .map(|(code, name, _t, calc_type, formula)| ItemSpec::new(*code, *name, *calc_type, *formula))
            .collect();
        specs.push(ItemSpec::new("SI_COMPANY_2", "单位社保(新)", CALC_TYPE_INPUT, None));
        specs.push(ItemSpec::new("HF_COMPANY_2", "单位公积金(新)", CALC_TYPE_INPUT, None));
        let plan = salary_vars::build_plan(&specs).expect("计划");
        let mut metas: Vec<ItemMeta> = rows
            .iter()
            .map(|(code, name, item_type, calc_type, formula)| {
                ItemMeta::new(*code, *name, *item_type, *calc_type, *formula)
            })
            .collect();
        metas.push(
            ItemMeta::new(
                "SI_COMPANY_2",
                "单位社保(新)",
                ITEM_TYPE_EMPLOYER,
                CALC_TYPE_INPUT,
                None,
            )
            .with_credit(Some(ACCOUNT_SI_COMPANY)),
        );
        metas.push(
            ItemMeta::new(
                "HF_COMPANY_2",
                "单位公积金(新)",
                ITEM_TYPE_EMPLOYER,
                CALC_TYPE_INPUT,
                None,
            )
            .with_credit(Some(ACCOUNT_HF_COMPANY)),
        );
        let engine = Engine::new(plan, metas);

        let mut input = zhangsan_input();
        input.manual.insert("SI_COMPANY_2".to_string(), amount("1680"));
        input.manual.insert("HF_COMPANY_2".to_string(), amount("960"));
        let slip = engine.compute(&input).expect("靠科目归位后要能算");
        assert_eq!(slip.si_company, amount("1680"));
        assert_eq!(slip.hf_company, amount("960"));
    }

    /// 新的单位承担项没有明细列 → 报错点名（不许猜一列塞进去）。
    #[test]
    fn unclassified_employer_item_is_reported() {
        let mut rows = seed_rows();
        rows.push(("UNION_FEE", "工会经费", ITEM_TYPE_EMPLOYER, CALC_TYPE_INPUT, None));
        let engine = engine_of(&rows);
        let mut input = zhangsan_input();
        input.manual.insert("UNION_FEE".to_string(), amount("50"));
        let issues = engine.compute(&input).expect_err("无列可用必须报");
        let message = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::UnclassifiedEmployer { .. }))
            .expect("必须报 UnclassifiedEmployer")
            .message();
        assert!(
            message.contains("UNION_FEE") && message.contains("工会经费"),
            "{message}"
        );
        assert!(
            message.contains("si_company") && message.contains("hf_company"),
            "{message}"
        );
    }

    /// 除零 / 未定义变量 → 点名「人 + 项」，不静默算 0。
    #[test]
    fn formula_error_names_employee_and_item() {
        let mut rows = seed_rows();
        rows.push((
            "DAILY",
            "日薪",
            ITEM_TYPE_EARNING,
            CALC_TYPE_FORMULA,
            Some("{BASE} / {ATT_SHOULD_DAYS}"),
        ));
        let engine = engine_of(&rows);
        let mut input = zhangsan_input();
        if let Some(numbers) = input.attendance.as_mut() {
            numbers.should_days = Decimal::ZERO;
        }
        let issues = engine.compute(&input).expect_err("除零必须报");
        let message = issues[0].message();
        assert!(matches!(issues[0], CalcIssue::Formula { .. }), "{issues:?}");
        assert!(message.contains("EMP-0001"), "{message}");
        assert!(message.contains("DAILY"), "{message}");
        assert!(message.contains("0"), "报文要能看出是除以 0：{message}");
    }

    // ---- 人工录入校验 ----

    #[test]
    fn manual_entries_are_validated_before_computing() {
        let engine = seed_engine();
        let employees = vec![head("EMP-0001", "张三")];
        let employee_id = employees[0].id;

        let ok = vec![ManualEntry {
            employee_id,
            item_code: "PERF".to_string(),
            amount: amount("1200"),
        }];
        assert!(engine.validate_manual(&ok, &employees).is_ok());

        // 编码不存在
        let issues = engine
            .validate_manual(
                &[ManualEntry {
                    employee_id,
                    item_code: "PERFF".to_string(),
                    amount: amount("1"),
                }],
                &employees,
            )
            .expect_err("错编码必须报");
        assert!(issues[0].message().contains("PERFF"), "{issues:?}");

        // 非 INPUT 项（FIXED）
        let issues = engine
            .validate_manual(
                &[ManualEntry {
                    employee_id,
                    item_code: "BASE".to_string(),
                    amount: amount("9000"),
                }],
                &employees,
            )
            .expect_err("FIXED 项不许手工填");
        assert!(
            issues[0].message().contains("不接受人工录入") && issues[0].message().contains("明细覆盖"),
            "{issues:?}"
        );

        // 同人同项两次
        let twice = vec![
            ManualEntry {
                employee_id,
                item_code: "PERF".to_string(),
                amount: amount("1200"),
            },
            ManualEntry {
                employee_id,
                item_code: "PERF".to_string(),
                amount: amount("1300"),
            },
        ];
        let issues = engine.validate_manual(&twice, &employees).expect_err("重复必须报");
        assert!(issues[0].message().contains("两次"), "{issues:?}");
    }

    /// 拼装：标准区间重叠 / 未知员工的人工录入 / 考勤缺失，一次全报。
    #[test]
    fn assemble_reports_overlap_and_unknown_manual_employee() {
        let employees = vec![head("EMP-0001", "张三"), head("EMP-0002", "李四")];
        let day = |text: &str| text.parse::<NaiveDate>().expect("日期");
        let standards = vec![
            StandardRow {
                employee_id: employees[0].id,
                item_code: "BASE".to_string(),
                amount: amount("8000"),
                effective_from: day("2026-01-01"),
            },
            StandardRow {
                employee_id: employees[0].id,
                item_code: "BASE".to_string(),
                amount: amount("9000"),
                effective_from: day("2026-11-01"),
            },
            StandardRow {
                employee_id: employees[1].id,
                item_code: "BASE".to_string(),
                amount: amount("7000"),
                effective_from: day("2026-01-01"),
            },
        ];
        let attendance: BTreeMap<Uuid, (Uuid, AttendanceNumbers)> = BTreeMap::from([(
            employees[1].id,
            (
                Uuid::now_v7(),
                AttendanceNumbers {
                    should_days: amount("22"),
                    ..AttendanceNumbers::default()
                },
            ),
        )]);
        let manual = vec![ManualEntry {
            employee_id: Uuid::now_v7(),
            item_code: "PERF".to_string(),
            amount: amount("100"),
        }];

        let issues = assemble(&employees, &standards, &attendance, &manual).expect_err("两处问题都要报");
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, CalcIssue::StandardOverlap { item_code, .. } if item_code == "BASE")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, CalcIssue::UnknownManualEmployee { .. })),
            "{issues:?}"
        );
        let overlap = issues
            .iter()
            .find(|issue| matches!(issue, CalcIssue::StandardOverlap { .. }))
            .expect("Overlap")
            .message();
        assert!(
            overlap.contains("2026-01-01") && overlap.contains("2026-11-01"),
            "{overlap}"
        );

        // 没有重叠时正常拼出：标准进 FIXED 项、考勤只在有的人身上
        let inputs = assemble(&employees, &standards[2..], &attendance, &[]).expect("正常拼装");
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1].standard.get("BASE"), Some(&amount("7000")), "李四的标准");
        assert!(!inputs[0].standard.contains_key("BASE"), "这批标准里没有张三的");
        assert!(inputs[0].attendance.is_none(), "张三没有考勤记录");
        assert!(inputs[1].attendance.is_some());
    }

    // ---- 空计划 / 汇总项缺失 ----

    /// 一个启用项都没有 → `NoActiveItems`（不是「算出全 0 的工资表」）。
    #[test]
    fn empty_plan_is_refused() {
        let specs = vec![ItemSpec::new("BASE", "基本工资", CALC_TYPE_FIXED, None)];
        let plan = salary_vars::build_plan(&specs).expect("计划");
        let engine = Engine::new(
            plan,
            [ItemMeta::new(
                "BASE",
                "基本工资",
                ITEM_TYPE_EARNING,
                CALC_TYPE_FIXED,
                None,
            )],
        );
        assert_eq!(engine.len(), 1);
        assert_eq!(engine.detail_item_count(), 1);

        let empty = Engine::new(salary_vars::build_plan(&[]).expect("空计划"), Vec::<ItemMeta>::new());
        let issues = empty
            .compute_all(&[PayInput::new(head("EMP-0001", "张三"))])
            .expect_err("空计划必须报");
        assert_eq!(issues, vec![CalcIssue::NoActiveItems]);
        assert!(issues[0].message().contains("薪资项配置"), "{}", issues[0].message());
    }

    /// 客户把 `GROSS` / `NET` 停用（不在计划里）→ 不做交叉校验，聚合值照样成立。
    #[test]
    fn missing_summary_items_do_not_block_computation() {
        let rows: Vec<_> = seed_rows()
            .into_iter()
            .filter(|row| row.0 != "GROSS" && row.0 != "NET")
            .collect();
        let engine = engine_of(&rows);
        let slip = engine.compute(&zhangsan_input()).expect("没有汇总项也要能算");
        assert_eq!(slip.gross_amount, amount("9954.03"));
        assert_eq!(slip.items.len(), 15, "15 个计算项都要落");
    }

    /// 批量：一个人算不出来不影响别人被算出来（问题一次收全）。
    #[test]
    fn compute_all_collects_issues_from_everyone() {
        let engine = seed_engine();
        let good = zhangsan_input();
        let mut bad = PayInput::new(head("EMP-0002", "李四"));
        bad.manual.insert("OTHER_WITHHOLD".to_string(), amount("99999"));
        bad.manual.insert("BASE".to_string(), amount("1")); // BASE 是 FIXED，无用但不会报
        bad.attendance = Some(AttendanceNumbers {
            should_days: amount("22"),
            ..AttendanceNumbers::default()
        });

        let issues = engine.compute_all(&[good, bad]).expect_err("有一个人错就整体失败");
        assert!(
            issues.iter().any(|issue| issue.message().contains("EMP-0002")),
            "{issues:?}"
        );
    }

    // ---- 期间 / 单号 / 精度 ----

    #[test]
    fn period_range_handles_month_ends_and_leap_years() {
        assert_eq!(
            period_range("2026-11").expect("合法"),
            (
                "2026-11-01".parse::<NaiveDate>().expect("日期"),
                "2026-11-30".parse::<NaiveDate>().expect("日期")
            )
        );
        assert_eq!(
            period_range("2024-02").expect("闰年").1,
            "2024-02-29".parse::<NaiveDate>().expect("日期")
        );
        assert_eq!(
            period_range("2026-12").expect("12 月").1,
            "2026-12-31".parse::<NaiveDate>().expect("日期")
        );
        for bad in ["2026-13", "2026-1", "26-11", "2026/11", "2026-11-01", ""] {
            assert!(period_range(bad).is_err(), "{bad} 不该被接受");
        }
        assert!(period_range("2026-13").expect_err("坏期间").msg.contains("yyyy-MM"));
    }

    #[test]
    fn sheet_no_uses_payroll_period_not_today() {
        let start = "2026-10-01".parse::<NaiveDate>().expect("日期");
        let end = "2026-10-31".parse::<NaiveDate>().expect("日期");
        assert_eq!(period_key_for("yyyyMM", start, end), "202610");
        assert_eq!(period_key_for("none", start, end), "GLOBAL");
        assert_eq!(period_key_for("yyyyMMdd", start, end), "20261031");
        assert_eq!(
            format_sheet_no("SAL", "-", "202610", 6, 1),
            "SAL-202610-000001",
            "设计 §3.1 ⑧ 的单号形态"
        );
        assert_eq!(format_sheet_no("SAL", "-", "202610", 6, 42), "SAL-202610-000042");
    }

    #[test]
    fn amounts_round_half_up_to_column_scale() {
        assert_eq!(round_to(amount("735.635"), 2), amount("735.64"));
        assert_eq!(round_to(amount("735.634"), 2), amount("735.63"));
        assert_eq!(round_amount(amount("0.12345")), amount("0.1235"));
        assert_eq!(round_amount(amount("0.12344")), amount("0.1234"));
        // 负数按「离开零」：财务口径的 -0.005 → -0.01
        assert_eq!(round_to(amount("-0.005"), 2), amount("-0.01"));
    }

    /// 插入用的占位符数量必须等于列数（否则 27 列 26 个参数，真库才炸）。
    #[test]
    fn batch_insert_placeholders_match_columns() {
        assert_eq!(DETAIL_TUPLE.matches('?').count(), DETAIL_COLUMNS);
        assert_eq!(ITEM_TUPLE.matches('?').count(), ITEM_COLUMNS);
        assert_eq!(DETAIL_SQL.matches(',').count() + 1, DETAIL_COLUMNS);
        assert_eq!(ITEM_SQL.matches(',').count() + 1, ITEM_COLUMNS);
    }

    #[test]
    fn long_notes_keep_the_result_tail() {
        let text = format!("{}= 999.99", "1 + ".repeat(400));
        let cut = truncate(&text, 40);
        assert!(cut.chars().count() <= 40, "{cut}");
        assert!(cut.ends_with("999.99"), "超长要保留尾部（结果在最后）：{cut}");
        assert_eq!(truncate("短说明 = 1", 40), "短说明 = 1");
    }
}
