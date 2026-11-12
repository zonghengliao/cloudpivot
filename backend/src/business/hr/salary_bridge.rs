//! 薪资单的**过账 + 审批回调桥**（`hr_salary` 流程，ARCH-9 §4.3 / §4.6 / §8.1 HR-39）。
//!
//! # 本卡干四件事
//!
//! | 能力 | 入口 | 谁调 |
//! |------|------|------|
//! | ① 过账：**两张凭证**（计提 + 发放） | [`post_salary`] | `salary_sheet_handler::post_sheet`（`POST /business/hr/salary/{id}/post`） |
//! | ② 审批回调：`APPROVED` → `SUBMITTED → APPROVED`（**不出凭证**）/ 驳回 → 回 `CALCULATED` | [`on_salary_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance, terminate_instance}`（**审批事务提交之后**） |
//! | ③ 净额法汇总（纯函数，可单测） | [`plan_accrual`] / [`plan_payment`] | 本文件的 [`post_salary`]，以及它的单测 |
//! | ④ 自审批守卫 + 实例终止 | [`guard_self_approval`] / [`cancel_approval_of`] | `flow::handler::approve_task`（事务前）/ `salary_sheet_handler::withdraw_sheet`（同事务） |
//!
//! # 净额法（设计 §4.6.2 第 1 条）为什么必须这么算
//!
//! `biz_voucher_entry` 的单边 `CHECK`（`V0149:100-110`）要求每行金额 **> 0**：不能写
//! 「借 5602.10 −200（考勤扣款）」这种负数行。而考勤扣款在 `PRE_DEDUCT` 里是**正数**，
//! 所以计提凭证的借方必须**按 `(费用科目, dept_id)` 汇总 `Σ EARNING − Σ PRE_DEDUCT`**：
//!
//! ```text
//! 张三：BASE 8000 + ATT_BONUS 200 = 收入 8200；ATT_DEDUCT 735.63 = 应发前扣减
//! 非净额法：借 5602.10 8200 / 借 5602.10 −735.63（撞 CHECK，写不进去）
//! 净额法：  借 5602.10 7464.37（= 8200 − 735.63），贷 2211.01 7464.37
//! ```
//!
//! 三条配套规则（逐条在 [`merge_signed`] / [`plan_accrual`] 里落）：
//!
//! 1. **净额 = 0 的组不落行**（否则撞单边 `CHECK`，也会让凭证虚增行数）；
//! 2. **净额 < 0 → 拒绝过账**（400/409，点名「哪个部门、哪个科目」）——「考勤扣款超过收入」
//!    是数据错误，不是可以用 0 糊过去的场景；糊过去等于把公司的支出算少了；
//! 3. **`amount = 0` 的薪资项不落行**（「全勤奖 0」在 `detail_item` 里有信息量，
//!    但在凭证里是一行噪声，且必然撞 `CHECK`）。
//!
//! # 发放凭证的最后一行（银行存款）是**倒挤**出来的
//!
//! ```text
//! 借 2211.01  应发合计
//!   贷 1002    实发（= 应发 − 代扣）
//!   贷 2241.03 社保个人 / 2241.04 公积金个人 / 2221.03 个税 / 1221 其他代扣
//! ```
//!
//! 银行存款不是一个薪资项（没有「银行」这个 `item_code`），所以它的金额 = 借方合计 −
//! 其他贷方合计。**倒挤之后必须再对一次账**：倒挤值 < 0 说明代扣合计大于应发
//! （数据错误）→ 拒绝过账；倒挤值 = 0 说明这个月一分钱都不用发（代扣刚好等于应发）→
//! 不落银行行（仍然是一张合法的两行凭证）。
//!
//! # 部门辅助核算（设计 §4.6.2 第 3 条）
//!
//! `5602.1x` / `2211.0x` 的 `aux_dimensions` 含 `DEPT`（HR-02 的 V0384 种的），所以
//! 借方与贷方都要带 `aux_dept_id`：既能出「全公司工资」，也能出「研发部工资」。
//! **只对声明了 `DEPT` 的科目带部门**（`2241.0x` / `2221.03` / `1221` / `1002` 不带）——
//! 给不带该维度的科目填部门，会让按部门汇总的辅助账多出一层永远对不上的切分。
//! 科目是否带 `DEPT` 由 [`dept_aware_accounts`] 从 `biz_account.aux_dimensions` 现查，
//! **不在代码里写死科目号**（客户可以自己加部门科目）。
//!
//! # 过账事务（设计 §4.6.4：不许留中间态）
//!
//! ```text
//! BEGIN
//!   SELECT … FROM biz_salary_sheet WHERE id=? FOR UPDATE
//!   闸：status = APPROVED / 考勤期间 LOCKED / 明细恒等式复算通过
//!   ① post_explicit_lines(计提凭证)  → accrual_voucher_id + no
//!   ② post_explicit_lines(发放凭证)  → payment_voucher_id + no
//!   ③ state_machine::transition(APPROVED → POSTED)（写 biz_status_log）
//!   ④ UPDATE biz_salary_sheet SET status='POSTED', 两张凭证 id/no, posted_at/by
//! COMMIT
//! ```
//!
//! 任何一步失败 → **整笔回滚**（两张凭证也一起消失），单据仍是 `APPROVED`，
//! 界面上的「过账」按钮还能再点一次。这就是 `AGENTS.md` 约定 11 要求的形态：
//! **不需要工程师改库就能恢复**。本函数没有 `tx.commit()` 之后的动作 ——
//! 过账不发通知、不调外部服务，所以不存在「提交后失败」的补偿分支；
//! 将来若加了通知，必须按约定 11 在失败路径上显式打回（注释留在 [`post_committed`]）。
//!
//! # 审批回调为什么不在这里出凭证
//!
//! 设计 §4.3 第 1 行写死：「薪资单 `SUBMITTED → APPROVED`（**不出凭证**）」。
//! 审批只决定「这一版金额被批准了」，凭证在**过账**那一步才生成（人工点「过账」）。
//! 把关账放在审批回调里，等于让一次点击同时完成「批准」与「记账」两件事，
//! 财务就没有「先看审批结果、再挑日期过账」的余地（设计 §4.8.1 的跨期发放需要这个余地）。
//!
//! # 幂等 / 陈旧回调
//!
//! 与 `hr::leave_bridge` 同款：回调要求 `biz_salary_sheet.approval_instance_id` **就是**
//! 这个实例（撤回后重提时旧实例的结论不采纳），`status` 必须是 `SUBMITTED`
//! （重复回调 / 已被撤回 → 幂等跳过）。

use std::collections::{BTreeMap, HashSet};

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher::{self, AccountRef};
use crate::business::erp::voucher_multi::{self, ExplicitLine, ExplicitVoucherInput};
use crate::business::flow::{engine, model as flow_model};
use crate::business::hr::salary_sheet_dto::{
    self as salary_dto, STATUS_APPROVED, STATUS_CALCULATED, STATUS_POSTED, STATUS_REVERSED, STATUS_SUBMITTED,
    SalarySheetCardRes,
};
use crate::business::hr::salary_sheet_model::{self as model, SheetRow};
use crate::business::shared::doc_kind::VoucherSourceType;
use crate::business::shared::state_machine::{self, ENTITY_SALARY_SHEET, Operator};
use crate::shared::error_code;

// ==================== 常量（与 V0394 种子 / ROUTE_CONTRACT 同口径） ====================

/// 薪资审批的业务类型（`biz_flow_def.biz_type`，与 `V0394` 种子逐字一致）。
pub const BIZ_TYPE: &str = "HR_SALARY";

/// 薪资审批的流程编码（`biz_flow_def.flow_code`，与 `V0394` 种子逐字一致）。
pub const FLOW_CODE: &str = "hr_salary";

/// 计提凭证的来源类型（`biz_voucher.source_type`，词表来自 HR-06 的 `doc_kind.rs`）。
pub const SOURCE_ACCRUAL: &str = VoucherSourceType::PayrollAccrual.as_str();

/// 发放凭证的来源类型。
pub const SOURCE_PAYMENT: &str = VoucherSourceType::PayrollPayment.as_str();

/// 银行存款的凭证规则编码（HR-02 的 `V0384__hr_accounts` 种的口径登记行）。
pub const RULE_PAYMENT: &str = "PAYROLL_PAYMENT_DEFAULT";

/// 规则缺失 / 失效时回落的银行存款科目编码（设计 §4.6.1 ② 写的就是 `1002`）。
pub const BANK_FALLBACK: &str = "1002";

/// 薪资项类型：收入（进应发，正向）。
pub const ITEM_EARNING: &str = "EARNING";
/// 薪资项类型：应发前扣减（进应发，负向 —— 净额法的主角）。
pub const ITEM_PRE_DEDUCT: &str = "PRE_DEDUCT";
/// 薪资项类型：代扣（形成负债，进发放凭证的贷方）。
pub const ITEM_WITHHOLD: &str = "WITHHOLD";
/// 薪资项类型：单位承担（进费用、不进应发）。
pub const ITEM_EMPLOYER: &str = "EMPLOYER";
/// 薪资项类型：汇总（只展示与校验，不落 `detail_item`）。
pub const ITEM_SUMMARY: &str = "SUMMARY";

// ==================== ① 净额法汇总（纯函数） ====================

/// 一条**参与过账**的明细项（由 [`load_item_amounts`] 从库里读出来，单测里手搓）。
///
/// 只带过账需要的东西：金额 + 借贷科目 + 部门（部门辅助核算用）+ 类型（决定正负）。
/// `item_name` 只用于报错文案（「部门 X 的『事假扣款』超过收入」比「科目 5602.10 为负」好懂）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAmount {
    /// 薪资项编码（报错 / 断言用）。
    pub item_code: String,
    /// 薪资项名称（报错文案用）。
    pub item_name: String,
    /// 薪资项类型（`EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER` / `SUMMARY`）。
    pub item_type: String,
    /// 借方科目编码（`EARNING` / `PRE_DEDUCT` / `EMPLOYER` 必填）。
    pub debit_code: Option<String>,
    /// 贷方科目编码（`EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER` 必填）。
    pub credit_code: Option<String>,
    /// 金额（`detail_item.amount`，允许负数 —— 补发 / 冲回）。
    pub amount: Decimal,
    /// 明细行快照的部门（`biz_salary_detail.dept_id`，可空）。
    pub dept_id: Option<Uuid>,
}

/// 一张凭证上的一行（**已按 `(科目, 部门)` 汇总**，借贷只有一个方向）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostLine {
    /// 科目编码。
    pub account_code: String,
    /// 部门辅助核算（只有声明了 `DEPT` 的科目才可能非空）。
    pub aux_dept_id: Option<Uuid>,
    /// 借方金额（与 `credit` 二选一）。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
}

impl PostLine {
    fn debit(account_code: impl Into<String>, aux_dept_id: Option<Uuid>, amount: Decimal) -> Self {
        Self {
            account_code: account_code.into(),
            aux_dept_id,
            debit: amount,
            credit: Decimal::ZERO,
        }
    }

    fn credit(account_code: impl Into<String>, aux_dept_id: Option<Uuid>, amount: Decimal) -> Self {
        Self {
            account_code: account_code.into(),
            aux_dept_id,
            debit: Decimal::ZERO,
            credit: amount,
        }
    }

    /// 本行的「方向金额」（借方为正、贷方为负）—— 只有单测与对账用。
    pub fn signed(&self) -> Decimal {
        self.debit - self.credit
    }
}

/// 净额法汇总的失败原因（纯数据，转 `ApiError` 由 [`plan_error`] 统一收口）。
///
/// 纯函数不直接造 `ApiError`：`ApiError` 带 HTTP 语义，而这里的每一条都是
/// 「数据错，不许记账」，与事务 / 请求无关 —— 分开才能被单测逐条钉住。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostPlanError {
    /// 某组净额为负（考勤扣款超过收入）。
    NegativeNet {
        /// 科目编码。
        account_code: String,
        /// 部门（`None` = 部门为空的那一组）。
        dept_id: Option<Uuid>,
        /// 净额（负数）。
        net: Decimal,
    },
    /// 参与过账的薪资项缺科目。
    MissingAccount {
        /// 薪资项编码。
        item_code: String,
        /// 缺哪一侧（`debit` / `credit`）。
        side: &'static str,
    },
    /// 一张凭证都没有行（薪资单金额全为 0）。
    NoLines,
    /// 倒挤出来的银行存款为负（代扣合计 > 应发合计）。
    NegativeBank {
        /// 倒挤值。
        amount: Decimal,
    },
    /// 借贷不平（构造上不可能，兜底防「将来改了汇总逻辑」）。
    Unbalanced {
        /// 借方合计。
        debit: Decimal,
        /// 贷方合计。
        credit: Decimal,
    },
}

impl PostPlanError {
    /// 人能读的报文（handler 直接透出，不暴露错误码）。
    pub fn message(&self) -> String {
        match self {
            PostPlanError::NegativeNet {
                account_code,
                dept_id,
                net,
            } => format!(
                "计提凭证借方为负：科目 {account_code} 在部门 {} 的净额为 {net}（收入 − 应发前扣减）。\
                 该部门的考勤扣款超过了收入，请先修正考勤 / 薪资标准再重算过账",
                dept_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "（未分配）".to_string())
            ),
            PostPlanError::MissingAccount { item_code, side } => format!(
                "薪资项 {item_code} 缺{}科目：过账要按科目生成凭证，请先在「薪资项」页补齐科目",
                if *side == "debit" { "借方" } else { "贷方" }
            ),
            PostPlanError::NoLines => "这张薪资单的金额全为 0（应发 0 / 代扣 0）：不构成凭证，不需要过账".to_string(),
            PostPlanError::NegativeBank { amount } => format!(
                "发放凭证的银行存款为 {amount}：代扣合计（社保 / 公积金 / 个税 / 其他）超过了应发合计，\
                 请先修正明细再重算过账"
            ),
            PostPlanError::Unbalanced { debit, credit } => format!(
                "工资凭证借贷不平（借 {debit} / 贷 {credit}）：汇总逻辑有问题，请联系研发\
                 （这一条是内部一致性兜底，正常不可达）"
            ),
        }
    }
}

/// 分组键：`(科目编码, 部门)`。用 `BTreeMap` 而不是 `HashMap`：分录顺序必须**确定**
/// （同一张单重复过账（红冲后重发）时，凭证行的排列一致，肉眼对账才不会被顺序骗）。
type GroupKey = (String, Option<Uuid>);

/// 一个方向上的金额累加器（借方或贷方）。
#[derive(Default)]
struct SideTotals {
    groups: BTreeMap<GroupKey, Decimal>,
}

impl SideTotals {
    /// 累加一笔；**零金额不建组**（设计 §4.6.2 第 2 条：`amount = 0` 的项不落行）。
    fn add(&mut self, key: GroupKey, amount: Decimal) {
        if amount.is_zero() {
            return;
        }
        *self.groups.entry(key).or_insert(Decimal::ZERO) += amount;
    }

    /// 落成「(科目, 部门) → 金额」：丢掉零净额的组，负净额报错（净额法的两条硬规则）。
    ///
    /// 返回的是**分组**而不是 [`PostLine`]：借方 / 贷方要分别落成两个方向的行，
    /// 方向由调用方决定（同一份 `SideTotals` 结构两种方向都用）。
    fn into_groups(
        self,
        negative: impl Fn(GroupKey, Decimal) -> PostPlanError,
    ) -> std::result::Result<Vec<(GroupKey, Decimal)>, PostPlanError> {
        let mut out = Vec::with_capacity(self.groups.len());
        for (key, amount) in self.groups {
            if amount.is_zero() {
                continue;
            }
            if amount < Decimal::ZERO {
                return Err(negative(key, amount));
            }
            out.push((key, amount));
        }
        Ok(out)
    }
}

/// 科目是否「带部门辅助核算」—— 决定这一行要不要填 `aux_dept_id`。
fn dept_key(code: &str, dept_id: Option<Uuid>, dept_aware: &HashSet<String>) -> GroupKey {
    (code.to_string(), if dept_aware.contains(code) { dept_id } else { None })
}

/// 计提凭证的**净额法**汇总（设计 §4.6.2）。
///
/// ```text
/// 借 5602.10 (按部门) Σ EARNING − Σ PRE_DEDUCT     ← 净额法：扣款直接抵减费用
/// 借 5602.11 (按部门) Σ EMPLOYER(社保单位)
/// 借 5602.12 (按部门) Σ EMPLOYER(公积金单位)
///   贷 2211.01 (按部门) Σ EARNING − Σ PRE_DEDUCT
///   贷 2211.02 (按部门) Σ EMPLOYER(社保单位)
///   贷 2211.03 (按部门) Σ EMPLOYER(公积金单位)
/// ```
///
/// `WITHHOLD`（代扣）**不进计提凭证**：那是「公司欠社保中心 / 税务」的负债，
/// 在**发放**凭证里才产生（发放时公司替员工扣下这笔钱，代缴之前挂在 2241.0x）。
/// 放进计提会让 2211 的贷方虚增（先全额计提再冲回），月末余额对不上。
pub fn plan_accrual(
    items: &[ItemAmount],
    dept_aware: &HashSet<String>,
) -> std::result::Result<Vec<PostLine>, PostPlanError> {
    let mut debits = SideTotals::default();
    let mut credits = SideTotals::default();
    for item in items {
        let sign = match item.item_type.as_str() {
            // 收入 / 单位承担 → 正；应发前扣减 → 负（净额法的核心：不写负数行，写净额）
            ITEM_EARNING | ITEM_EMPLOYER => Decimal::ONE,
            ITEM_PRE_DEDUCT => -Decimal::ONE,
            // 代扣不在计提凭证里；汇总项本来就不落 detail_item（防御性跳过）
            ITEM_WITHHOLD | ITEM_SUMMARY => continue,
            _ => continue,
        };
        if item.amount.is_zero() {
            continue;
        }
        let debit = item
            .debit_code
            .as_deref()
            .ok_or_else(|| PostPlanError::MissingAccount {
                item_code: item.item_code.clone(),
                side: "debit",
            })?;
        let credit = item
            .credit_code
            .as_deref()
            .ok_or_else(|| PostPlanError::MissingAccount {
                item_code: item.item_code.clone(),
                side: "credit",
            })?;
        let amount = item.amount * sign;
        debits.add(dept_key(debit, item.dept_id, dept_aware), amount);
        credits.add(dept_key(credit, item.dept_id, dept_aware), amount);
    }
    let mut lines: Vec<PostLine> = Vec::new();
    for (key, amount) in debits.into_groups(|(code, dept), net| PostPlanError::NegativeNet {
        account_code: code,
        dept_id: dept,
        net,
    })? {
        lines.push(PostLine::debit(key.0, key.1, amount));
    }
    for (key, amount) in credits.into_groups(|(code, dept), net| PostPlanError::NegativeNet {
        account_code: code,
        dept_id: dept,
        net,
    })? {
        lines.push(PostLine::credit(key.0, key.1, amount));
    }
    if lines.is_empty() {
        return Err(PostPlanError::NoLines);
    }
    check_balanced(&lines)?;
    Ok(lines)
}

/// 发放凭证的汇总（设计 §4.6.1 ② + 本节「倒挤银行存款」）。
///
/// ```text
/// 借 2211.01 (按部门) Σ EARNING − Σ PRE_DEDUCT   ← 应发合计（把计提的负债冲掉）
///   贷 1002          应发 − 代扣                  ← 倒挤：实发
///   贷 2241.03 / 2241.04 / 2221.03 / 1221  代扣各项（按代扣项的贷方科目分）。
/// ```
///
/// `bank_code` 由调用方给（凭证规则 `PAYROLL_PAYMENT_DEFAULT` 的贷方，缺失回落 `1002`）：
/// 纯函数不查库，客户换开户行只改规则。
pub fn plan_payment(
    items: &[ItemAmount],
    dept_aware: &HashSet<String>,
    bank_code: &str,
) -> std::result::Result<Vec<PostLine>, PostPlanError> {
    let mut debits = SideTotals::default();
    let mut credits = SideTotals::default();
    let mut withhold_total = Decimal::ZERO;
    for item in items {
        if item.amount.is_zero() {
            continue;
        }
        match item.item_type.as_str() {
            ITEM_EARNING | ITEM_PRE_DEDUCT => {
                let sign = if item.item_type == ITEM_EARNING {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                let credit = item
                    .credit_code
                    .as_deref()
                    .ok_or_else(|| PostPlanError::MissingAccount {
                        item_code: item.item_code.clone(),
                        side: "credit",
                    })?;
                let amount = item.amount * sign;
                // 借：冲掉计提时挂的应付职工薪酬（按部门，与计提同口径）
                debits.add(dept_key(credit, item.dept_id, dept_aware), amount);
            }
            ITEM_WITHHOLD => {
                let credit = item
                    .credit_code
                    .as_deref()
                    .ok_or_else(|| PostPlanError::MissingAccount {
                        item_code: item.item_code.clone(),
                        side: "credit",
                    })?;
                credits.add(dept_key(credit, item.dept_id, dept_aware), item.amount);
                withhold_total += item.amount;
            }
            // 单位承担不进发放凭证（那是公司自己的费用，已经计提且不发现金）
            ITEM_EMPLOYER | ITEM_SUMMARY => {}
            _ => {}
        }
    }
    let mut lines: Vec<PostLine> = Vec::new();
    let mut debit_total = Decimal::ZERO;
    for (key, amount) in debits.into_groups(|(code, dept), net| PostPlanError::NegativeNet {
        account_code: code,
        dept_id: dept,
        net,
    })? {
        debit_total += amount;
        lines.push(PostLine::debit(key.0, key.1, amount));
    }
    if lines.is_empty() {
        // 应发全为 0（只有代扣 / 单位承担）：没有可发放的工资
        return Err(PostPlanError::NoLines);
    }
    for (key, amount) in credits.into_groups(|(code, dept), net| PostPlanError::NegativeNet {
        account_code: code,
        dept_id: dept,
        net,
    })? {
        lines.push(PostLine::credit(key.0, key.1, amount));
    }
    // 倒挤银行存款（非部门科目 → 单行，不带 aux_dept_id）
    let bank = debit_total - withhold_total;
    if bank < Decimal::ZERO {
        return Err(PostPlanError::NegativeBank { amount: bank });
    }
    if !bank.is_zero() {
        lines.push(PostLine::credit(bank_code, None, bank));
    }
    check_balanced(&lines)?;
    Ok(lines)
}

/// 借贷必须相等（构造上必然相等，这里是给「将来有人改汇总逻辑」兜底）。
fn check_balanced(lines: &[PostLine]) -> std::result::Result<(), PostPlanError> {
    let mut debit = Decimal::ZERO;
    let mut credit = Decimal::ZERO;
    for line in lines {
        debit += line.debit;
        credit += line.credit;
    }
    if debit != credit {
        return Err(PostPlanError::Unbalanced { debit, credit });
    }
    Ok(())
}

/// 纯函数的失败 → `409 + 68008`（业务规则拦截，报文直出给用户）。
pub fn plan_error(error: PostPlanError) -> ApiError {
    ApiError::new(409, error_code::HR_SALARY_RULE_BLOCKED, error.message())
}

// ==================== ② 取数（真库） ====================

/// 一条凭证的来源（单号 + 日期 + 摘要）—— 两张凭证各一份。
struct SourceDoc<'a> {
    sheet: &'a SheetRow,
    /// 凭证日期（计提 = 薪资期间末日；发放 = 过账日）。
    voucher_date: NaiveDate,
    /// 凭证摘要。
    summary: String,
}

/// 读这张单**参与过账**的明细项（带部门 + 借贷科目）。
///
/// 科目取 `biz_salary_item`（配置表）：薪资项被删（`item_id` 为 `NULL`）时按 `item_code`
/// 再兜一次 —— 明细项的科目是「配置快照」的语义，不能在删项之后让历史单据过不了账。
/// 两处都没有（项被删干净了）→ 科目的 `Option` 为 `None`，由纯函数报
/// [`PostPlanError::MissingAccount`]（点名声薪项编码）。
async fn load_item_amounts(ex: &impl SqlExecutor, sheet_id: Uuid) -> DbResult<Vec<ItemAmount>> {
    // SCOPE-EXEMPT: 过账按薪资单主键取它的明细项（单据内聚读，不做数据范围裁剪）。
    let sql = "SELECT i.item_code, i.item_name, i.item_type, i.amount, d.dept_id, \
                      COALESCE(si.debit_account_code, sc.debit_account_code)  AS debit_code, \
                      COALESCE(si.credit_account_code, sc.credit_account_code) AS credit_code \
                 FROM biz_salary_detail_item i \
                 JOIN biz_salary_detail d ON d.id = i.detail_id \
                 LEFT JOIN biz_salary_item si \
                        ON si.id = i.item_id AND si.deleted_at IS NULL \
                 LEFT JOIN biz_salary_item sc \
                        ON sc.item_code = i.item_code AND sc.org_id = i.org_id AND sc.deleted_at IS NULL \
                WHERE i.sheet_id = ?::uuid \
                ORDER BY i.line_no, i.item_code, d.employee_no";
    let rows = ex.query_sql(sql.to_string(), vec![Value::from(&sheet_id)]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(ItemAmount {
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_type: row.get_typed("item_type")?,
            debit_code: row.get_typed("debit_code")?,
            credit_code: row.get_typed("credit_code")?,
            amount: row.get_typed("amount")?,
            dept_id: row.get_typed("dept_id")?,
        });
    }
    Ok(out)
}

/// 声明了 `DEPT` 辅助核算的科目编码集合（决定凭证行要不要带部门）。
async fn dept_aware_accounts(ex: &impl SqlExecutor) -> DbResult<HashSet<String>> {
    // SCOPE-EXEMPT: 科目表是全局主数据，这里列表头（编码 + 维度），不按数据范围裁。
    let rows = ex
        .query_sql(
            "SELECT account_code FROM biz_account \
              WHERE deleted_at IS NULL AND status = 1 AND upper(coalesce(aux_dimensions, '')) LIKE '%DEPT%'"
                .to_string(),
            vec![],
        )
        .await?;
    let mut out = HashSet::with_capacity(rows.len());
    for row in rows {
        out.insert(row.get_typed::<String>("account_code")?);
    }
    Ok(out)
}

/// 银行存款科目：凭证规则 `PAYROLL_PAYMENT_DEFAULT` 的贷方 → 缺失/失效回落 `1002`。
///
/// 为什么两级：规则是**口径登记**（HR-02 的 V0384，配置页可见、客户可改开户行），
/// 但客户把银行科目裂成 `1002.01 工行 / 1002.02 建行` 之后规则还指着父科目 `1002`，
/// 这时 `ensure_leaf` 会拒 —— 直接 409 会把整张工资单卡死。所以退回按编码取 1002，
/// 两级都不可用时由 [`voucher_multi::leaf_required`] 报「缺哪个科目」（运维能看懂）。
async fn bank_account(ex: &impl SqlExecutor) -> std::result::Result<AccountRef, ApiError> {
    match voucher_multi::rule_accounts(ex, RULE_PAYMENT).await {
        Ok(Some(accounts)) => return Ok(accounts.credit),
        Ok(None) => tracing::warn!(
            rule_code = RULE_PAYMENT,
            "工资凭证规则不存在：发放凭证的银行存款回落按编码 {} 取末级科目",
            BANK_FALLBACK
        ),
        Err(error) => tracing::warn!(
            rule_code = RULE_PAYMENT,
            %error,
            "工资凭证规则不可用：发放凭证的银行存款回落按编码 {} 取末级科目",
            BANK_FALLBACK
        ),
    }
    voucher_multi::leaf_required(ex, BANK_FALLBACK)
        .await
        .map_err(|error| voucher_problem(error, "银行存款"))
}

/// 计划里的科目编码 → 末级科目快照（非末级 / 停用 → 400 点名科目）。
async fn resolve_lines(
    ex: &impl SqlExecutor,
    planned: &[PostLine],
) -> std::result::Result<Vec<ExplicitLine>, ApiError> {
    let mut cache: BTreeMap<String, AccountRef> = BTreeMap::new();
    let mut out = Vec::with_capacity(planned.len());
    for line in planned {
        let account = match cache.get(&line.account_code) {
            Some(account) => account.clone(),
            None => {
                let account = voucher_multi::leaf_required(ex, &line.account_code)
                    .await
                    .map_err(|error| voucher_problem(error, "工资凭证科目"))?;
                cache.insert(line.account_code.clone(), account.clone());
                account
            }
        };
        let mut explicit = if line.debit > Decimal::ZERO {
            ExplicitLine::debit(account, line.debit)
        } else {
            ExplicitLine::credit(account, line.credit)
        };
        explicit.aux_dept_id = line.aux_dept_id;
        out.push(explicit);
    }
    Ok(out)
}

// ==================== ③ 过账 ====================

/// 过账（设计 §5.2 #43 / §4.6.4）：生成计提 + 发放两张凭证并把单据推到 `POSTED`。
///
/// # 闸门（全部在同一个事务里，越早拒绝越好）
///
/// 1. 单据存在且未软删（否则 404）；
/// 2. `status = APPROVED`（审批通过才许记账 —— 否则 409 点明当前状态与下一步动作）；
/// 3. 数据来源的考勤期间 `LOCKED`（设计 §4.8.3：不许按未定稿的考勤发钱）；
/// 4. **明细恒等式复算**：`SUM(明细.gross_amount)` = 单头 `gross_total` 等 7 个列
///    （巡检 A6 的在线版 —— 让「有人绕过引擎改过库」在过账这一刻被拦下，而不是等到月结）；
/// 5. 两张凭证借贷平衡（纯函数算 + `post_voucher` 再算一遍）。
///
/// # 为什么先计提后发放
///
/// 顺序不影响余额（两张都在同一个事务、同一批数据），但**凭证号**会受影响：
/// 同一天过账时计提拿到小号、发放拿到大号，人翻凭证簿看到的是「先计提后发放」，
/// 与业务直觉一致。
pub async fn post_salary(operator: Operator, sheet_id: Uuid) -> std::result::Result<SalarySheetCardRes, ApiError> {
    let now: DateTime<Utc> = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启薪资过账事务"))?;
    let outcome = post_in_tx(&tx, operator, sheet_id, now).await;
    match outcome {
        Ok(ids) => {
            tx.commit().await.map_err(|error| internal(error, "提交薪资过账事务"))?;
            // ★ 提交之后只做「读回来给人看」：不写库、不发通知。
            //   将来若在这里加了通知 / 外部调用，失败必须按 AGENTS.md 约定 11 打回 POSTED →
            //   （不允许）—— 更正确的做法是把这类动作做成「可重试的补偿任务」，不要塞在这里。
            post_committed(sheet_id, ids).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 过账事务体：返回两张凭证 id（计提, 发放）。
async fn post_in_tx(
    tx: &Transaction,
    operator: Operator,
    sheet_id: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<(Uuid, Uuid), ApiError> {
    // ① 锁单：并发过账只能有一次生效（后到的等锁，看到 POSTED 后被 ② 挡住）
    let Some(sheet) = model::lock_sheet(tx, sheet_id)
        .await
        .map_err(|error| internal(error, "锁定薪资单"))?
    else {
        return Err(not_found(sheet_id));
    };
    // ② 状态闸
    if sheet.status != STATUS_APPROVED {
        return Err(status_blocked(&sheet));
    }
    // ③ 考勤期间必须锁定（DRAFT / SUBMITTED 期间的数据还会变）
    ensure_attendance_locked(tx, &sheet).await?;
    // ④ 明细恒等式复算（A6 的在线版）
    ensure_totals_match(tx, &sheet).await?;
    // ⑤ 算两张凭证（净额法）
    let items = load_item_amounts(tx, sheet_id)
        .await
        .map_err(|error| internal(error, "读取薪资明细项"))?;
    let dept_aware = dept_aware_accounts(tx)
        .await
        .map_err(|error| internal(error, "读取科目的辅助核算维度"))?;
    let accrual = plan_accrual(&items, &dept_aware).map_err(plan_error)?;
    let bank = bank_account(tx).await?;
    let payment = plan_payment(&items, &dept_aware, &bank.code).map_err(plan_error)?;
    // ⑥ 落两张凭证（共享 `post_voucher` 的取号 / 平衡 / 期间闸 / 分录落库）
    let accrual_date = sheet.period_end;
    let payment_date = voucher::today_cst(now);
    let accrual_id = post_one(
        tx,
        &SourceDoc {
            sheet: &sheet,
            voucher_date: accrual_date,
            summary: format!("计提 {} 工资 {}", sheet.period_code, sheet.sheet_no),
        },
        SOURCE_ACCRUAL,
        &accrual,
        operator.id,
        now,
    )
    .await?;
    let payment_id = post_one(
        tx,
        &SourceDoc {
            sheet: &sheet,
            voucher_date: payment_date,
            summary: format!("发放 {} 工资 {}", sheet.period_code, sheet.sheet_no),
        },
        SOURCE_PAYMENT,
        &payment,
        operator.id,
        now,
    )
    .await?;
    // ⑦ 两张凭证号（单据上要存「人能查的号」，不只是 id）
    let accrual_no = voucher_no(tx, accrual_id).await?;
    let payment_no = voucher_no(tx, payment_id).await?;
    // ⑧ 状态机（唯一入口：边校验 + 写 biz_status_log）
    state_machine::transition(
        tx,
        ENTITY_SALARY_SHEET,
        sheet.id,
        &sheet.sheet_no,
        &state_machine::SALARY_SHEET_EDGES,
        STATUS_APPROVED,
        STATUS_POSTED,
        operator.clone(),
        Some(&format!(
            "过账：计提凭证 {accrual_no} + 发放凭证 {payment_no}（净额法，借贷各 {} 元）",
            sheet.gross_total
        )),
        None,
    )
    .await?;
    // ⑨ CAS 回写（状态机只管日志，不回写业务表）
    let affected = model::mark_posted(
        tx,
        sheet.id,
        (accrual_id, &accrual_no),
        (payment_id, &payment_no),
        operator.id,
    )
    .await
    .map_err(|error| write_error(error, "回写薪资单过账结果", &sheet.sheet_no))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("薪资单 {} 的状态在过账期间已被改动，请刷新后重试", sheet.sheet_no),
        ));
    }
    tracing::info!(
        sheet_id = %sheet.id,
        sheet_no = %sheet.sheet_no,
        %accrual_id,
        accrual_no,
        %payment_id,
        payment_no,
        "薪资单已过账（计提 + 发放两张凭证）"
    );
    Ok((accrual_id, payment_id))
}

/// 落一张工资凭证。
async fn post_one(
    tx: &Transaction,
    doc: &SourceDoc<'_>,
    source_type: &str,
    planned: &[PostLine],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    let lines = resolve_lines(tx, planned).await?;
    voucher_multi::post_explicit_lines(
        tx,
        ExplicitVoucherInput {
            source_type,
            source_id: Some(doc.sheet.id),
            source_no: &doc.sheet.sheet_no,
            voucher_date: doc.voucher_date,
            summary: &doc.summary,
            lines: &lines,
            remark: None,
            operator,
            now,
        },
    )
    .await
    .map_err(|error| voucher_problem(error, "生成工资凭证"))
}

/// 提交之后的收尾（只是读回来给人看）。
async fn post_committed(sheet_id: Uuid, ids: (Uuid, Uuid)) -> std::result::Result<SalarySheetCardRes, ApiError> {
    let sheet = model::find_sheet(sheet_id)
        .await
        .map_err(|error| internal(error, "读回过账后的薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;
    debug_assert_eq!(sheet.accrual_voucher_id, Some(ids.0));
    debug_assert_eq!(sheet.payment_voucher_id, Some(ids.1));
    Ok(sheet.to_card())
}

/// 数据来源的考勤期间必须已锁定（设计 §4.8.3 / 巡检 A8 的前置）。
///
/// 没有挂考勤期间（`attendance_period_id IS NULL`：手工建单 / 客户不用考勤）→ 放行：
/// 那是设计允许的「无考勤口径」，不能因为没有考勤就不许发工资。
async fn ensure_attendance_locked(ex: &impl SqlExecutor, sheet: &SheetRow) -> std::result::Result<(), ApiError> {
    let Some(period_id) = sheet.attendance_period_id else {
        tracing::warn!(
            sheet_no = %sheet.sheet_no,
            "薪资单没有关联考勤期间（手工 / 无考勤口径）：过账不做考勤锁定校验"
        );
        return Ok(());
    };
    // SCOPE-EXEMPT: 按主键读 1 行期间状态。
    let row = ex
        .query_first_sql(
            "SELECT period_code, status FROM biz_attendance_period WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await
        .map_err(|error| internal(error, "读取考勤期间"))?;
    let Some(row) = row else {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 的数据来源考勤期间 {} 已删除：请重新锁定考勤期间后重算再提交",
                sheet.sheet_no, period_id
            ),
        ));
    };
    let status: String = row
        .get_typed("status")
        .map_err(|error| internal(error, "读考勤期间状态"))?;
    let period_code: String = row
        .get_typed("period_code")
        .map_err(|error| internal(error, "读考勤期间编码"))?;
    if status != "LOCKED" {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 的考勤期间 {period_code} 还没锁定（当前 {status}）：\
                 过账要求考勤已定稿（否则会出现「工资按 A 版算、考勤改成 B 版」）。\
                 请先在「考勤台账」锁定该期间",
                sheet.sheet_no
            ),
        ));
    }
    Ok(())
}

/// 明细恒等式复算（巡检 A6 的在线版）：`SUM(明细)` 必须等于单头 7 个金额列。
///
/// 只对 6 个「明细有对应列」的口径做对账（`employee_count` 一起）：`gross_total` 等
/// 已由库层 `CHECK` 与单头内部自洽绑定，那一层是「单头自己算得对不对」；
/// 这一层是「明细加起来是不是单头」—— 跨表，**没有任何约束能管**（设计 §4.9 A6）。
async fn ensure_totals_match(ex: &impl SqlExecutor, sheet: &SheetRow) -> std::result::Result<(), ApiError> {
    // SCOPE-EXEMPT: 单据内聚读（按 sheet_id 聚合它的明细）。
    let row = ex
        .query_first_sql(
            "SELECT count(*)::int AS cnt, \
                    COALESCE(SUM(earning_amount), 0)  AS earning, \
                    COALESCE(SUM(pre_deduct_amount), 0) AS pre_deduct, \
                    COALESCE(SUM(gross_amount), 0)    AS gross, \
                    COALESCE(SUM(withhold_amount), 0) AS withhold, \
                    COALESCE(SUM(net_amount), 0)      AS net, \
                    COALESCE(SUM(si_company), 0)      AS si, \
                    COALESCE(SUM(hf_company), 0)      AS hf \
               FROM biz_salary_detail WHERE sheet_id = ?::uuid"
                .to_string(),
            vec![Value::from(&sheet.id)],
        )
        .await
        .map_err(|error| internal(error, "复算薪资明细合计"))?;
    let Some(row) = row else {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!("薪资单 {} 的明细复算失败：查不到明细合计", sheet.sheet_no),
        ));
    };
    let mut mismatches: Vec<String> = Vec::new();
    let cnt: i32 = row.get_typed("cnt").map_err(|error| internal(error, "读明细行数"))?;
    if cnt != sheet.employee_count {
        mismatches.push(format!("人数 明细 {cnt} ≠ 单头 {}", sheet.employee_count));
    }
    let pairs: [(&str, &str, Decimal); 7] = [
        ("收入合计", "earning", sheet.earning_total),
        ("应发前扣减", "pre_deduct", sheet.pre_deduct_total),
        ("应发合计", "gross", sheet.gross_total),
        ("代扣合计", "withhold", sheet.withhold_total),
        ("实发合计", "net", sheet.net_total),
        ("单位社保", "si", sheet.si_company_total),
        ("单位公积金", "hf", sheet.hf_company_total),
    ];
    for (label, column, header) in pairs {
        let detail: Decimal = row
            .get_typed(column)
            .map_err(|error| internal(error, "读明细金额合计"))?;
        if detail != header {
            mismatches.push(format!("{label} 明细 {detail} ≠ 单头 {header}"));
        }
    }
    if !mismatches.is_empty() {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 的明细与单头对不上（{}），过账会出不平的账：请重新试算后再提交。\
                 巡检 A6 的在线版就是这道闸",
                sheet.sheet_no,
                mismatches.join("；")
            ),
        ));
    }
    Ok(())
}

/// 读凭证号（单据上存「人能查的号」）。
async fn voucher_no(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<String, ApiError> {
    let Some(row) = voucher::find_voucher(ex, id)
        .await
        .map_err(|error| internal(error, "读取凭证"))?
    else {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("凭证 {id} 刚落库就查不到：请刷新薪资单确认（若凭证缺失请联系研发）"),
        ));
    };
    Ok(row.voucher_no)
}

// ==================== ③·5 红冲（HR-40） ====================

/// 红冲（设计 §4.8.2 / §5.2 #44）：把「计提 + 发放」两张凭证**各冲一张红字凭证**，
/// 单据 `POSTED → REVERSED`（终态）。
///
/// # 两张红字凭证从哪来
///
/// 不复刻红冲算法：逐张调 `voucher::reverse_posted`（`erp/voucher.rs`），它已经钉住了
/// 全部库层细节 —— 借贷互换、金额恒正、`is_red=1`、`reverse_of=原凭证`、原凭证回写
/// `reversed_by`、独立号段 `PZR-yyyyMMdd-NNNNNN`、期间已结账则拒绝。
/// 本卡只负责**把两张串起来 + 状态机 + 回写单据**。手写一遍分录 = 两套红冲口径，
/// 改一侧漏另一侧是账务模块最贵的 bug（与 HR-39 过账走 `post_explicit_lines` 同一取向）。
///
/// # 顺序（同一事务，任一步失败整笔回滚）
///
/// ```text
/// BEGIN
///   SELECT … FROM biz_salary_sheet WHERE id=? FOR UPDATE        锁单
///   闸① status = POSTED（只有已过账的单有账可冲）
///   闸② accrual_voucher_id / payment_voucher_id 都在（A8 的前置）
///   SELECT … FROM biz_voucher … FOR UPDATE ×2                    锁两张原凭证
///   ① reverse_posted(计提凭证) → red_accrual（PZR-…）
///   ② reverse_posted(发放凭证) → red_payment（PZR-…）
///   ③ state_machine::transition(POSTED → REVERSED)（必填原因边 + 写 biz_status_log）
///   ④ UPDATE biz_salary_sheet SET status='REVERSED', reversal_voucher_id=①,
///                                reverse_reason, reversed_at/by
/// COMMIT
/// ```
///
/// `reversal_voucher_id` **只记第一张**（计提凭证的红字）；第二张通过
/// `biz_voucher.reverse_of = payment_voucher_id` 反查（设计 §4.8.2 的既定口径）。
///
/// # 提交之后不做任何写动作
///
/// 红冲不发通知、不调外部服务；`COMMIT` 之后只把单据读回来给人看。因此不存在
/// 「提交后失败」需要补偿的分支（`AGENTS.md` 约定 11）：失败一律发生在事务内 →
/// **两张红字凭证一起消失**，单据仍是 `POSTED`，界面上「红冲」按钮还能再点一次。
pub async fn reverse_salary(
    operator: Operator,
    sheet_id: Uuid,
    reason: &str,
) -> std::result::Result<SalarySheetCardRes, ApiError> {
    let reason = reason.trim();
    // DTO 层（`salary_dto::parse_reverse_reason`）已经校验过长度；这里只兜「内部直接调
    // bridge 绕过 handler」的路径 —— 原因必填是本卡的验收项，不能在入口漏一道。
    if reason.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "reason 必填：红冲必须写明原因（会落单据、状态日志与两张红字凭证）",
        ));
    }
    let now: DateTime<Utc> = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启薪资红冲事务"))?;
    let outcome = reverse_in_tx(&tx, operator, sheet_id, reason, now).await;
    match outcome {
        Ok(reversal_id) => {
            tx.commit().await.map_err(|error| internal(error, "提交薪资红冲事务"))?;
            reverse_committed(sheet_id, reversal_id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 红冲事务体：返回第一张红字凭证 id（计提凭证的红字）。
async fn reverse_in_tx(
    tx: &Transaction,
    operator: Operator,
    sheet_id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    // ① 锁单：并发两次红冲只有一次生效（后到的等锁，看到 REVERSED 后被 ② 挡住）
    let Some(sheet) = model::lock_sheet(tx, sheet_id)
        .await
        .map_err(|error| internal(error, "锁定薪资单"))?
    else {
        return Err(not_found(sheet_id));
    };
    // ② 状态闸：只有 POSTED 有账可冲
    if sheet.status != STATUS_POSTED {
        return Err(reverse_blocked(&sheet));
    }
    // ③ A8 的前置：POSTED 单必然有这两列；缺了说明有人绕过过账写过库
    let (Some(accrual_id), Some(payment_id)) = (sheet.accrual_voucher_id, sheet.payment_voucher_id) else {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 是「已过账」但记不到计提 / 发放凭证（巡检 A8 的不合格行）：\
                 红冲无从下手，请让研发核对这张单的两张凭证（本卡不允许凭金额再造凭证）",
                sheet.sheet_no
            ),
        ));
    };
    // ④ 锁两张原凭证（凭证台账页也能单张红冲，同一条焊缝上只许一个赢家）
    let accrual = lock_original(tx, &sheet, accrual_id, SOURCE_ACCRUAL).await?;
    let payment = lock_original(tx, &sheet, payment_id, SOURCE_PAYMENT).await?;
    // ⑤ 各冲一张红字：日期取「今天」（CST）——红冲是更正动作，记在发现问题的期间
    let voucher_date = voucher::today_cst(now);
    let accrual_red = reverse_one(tx, &accrual, reason, voucher_date, operator.id, now, "计提").await?;
    let payment_red = reverse_one(tx, &payment, reason, voucher_date, operator.id, now, "发放").await?;
    let accrual_red_no = voucher_no(tx, accrual_red).await?;
    let payment_red_no = voucher_no(tx, payment_red).await?;
    // ⑥ 状态机（唯一入口：边校验 + 必填原因 + 写 biz_status_log）
    state_machine::transition(
        tx,
        ENTITY_SALARY_SHEET,
        sheet.id,
        &sheet.sheet_no,
        &state_machine::SALARY_SHEET_EDGES,
        STATUS_POSTED,
        STATUS_REVERSED,
        operator.clone(),
        Some(&format!(
            "红冲：{reason}（红字凭证 {accrual_red_no} + {payment_red_no}）"
        )),
        None,
    )
    .await?;
    // ⑦ CAS 回写（状态机只管日志，不回写业务表）
    let affected = model::mark_reversed(tx, sheet.id, accrual_red, reason, operator.id)
        .await
        .map_err(|error| write_error(error, "回写薪资单红冲结果", &sheet.sheet_no))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("薪资单 {} 的状态在红冲期间已被改动，请刷新后重试", sheet.sheet_no),
        ));
    }
    tracing::info!(
        sheet_id = %sheet.id,
        sheet_no = %sheet.sheet_no,
        %accrual_red,
        accrual_red_no,
        %payment_red,
        payment_red_no,
        reason,
        "薪资单已红冲（计提 + 发放两张红字凭证）"
    );
    Ok(accrual_red)
}

/// 锁一张原凭证，并核对它确实是这张薪资单的那张（防「单据指错了凭证」）。
async fn lock_original(
    tx: &Transaction,
    sheet: &SheetRow,
    voucher_id: Uuid,
    expect_source: &str,
) -> std::result::Result<voucher::VoucherRow, ApiError> {
    let Some(row) = voucher::find_voucher_locked(tx, voucher_id)
        .await
        .map_err(|error| internal(error, "锁定薪资凭证"))?
    else {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 记着凭证 {voucher_id}，但这张凭证不存在（可能被删）：\
                 红冲无法进行，请让研发核对凭证台账",
                sheet.sheet_no
            ),
        ));
    };
    if row.source_id != Some(sheet.id) || row.source_type != expect_source {
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 记着的凭证 {} 来源是 {}/{}，与单据 {}/{} 不符：\
                 红冲会冲错账，已拦下（请让研发核对）",
                sheet.sheet_no,
                row.voucher_no,
                row.source_type,
                row.source_id.map(|id| id.to_string()).unwrap_or_default(),
                expect_source,
                sheet.id
            ),
        ));
    }
    Ok(row)
}

/// 冲一张凭证：`reverse_posted` 的回写失败在这里翻成人话 409。
async fn reverse_one(
    tx: &Transaction,
    original: &voucher::VoucherRow,
    reason: &str,
    voucher_date: NaiveDate,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
    label: &str,
) -> std::result::Result<Uuid, ApiError> {
    voucher::reverse_posted(
        tx,
        original,
        voucher::ReversalInput {
            reason: Some(reason),
            voucher_date,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| match error {
        DbError::InvalidValue(message) => ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!("{label}凭证红冲失败：{message}"),
        ),
        other => internal(other, "生成红字凭证"),
    })
}

/// 提交之后的收尾（只是读回来给人看；不写库、不发通知）。
async fn reverse_committed(sheet_id: Uuid, reversal_id: Uuid) -> std::result::Result<SalarySheetCardRes, ApiError> {
    let sheet = model::find_sheet(sheet_id)
        .await
        .map_err(|error| internal(error, "读回红冲后的薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;
    debug_assert_eq!(sheet.reversal_voucher_id, Some(reversal_id));
    debug_assert_eq!(sheet.status, STATUS_REVERSED);
    Ok(sheet.to_card())
}

/// 红冲的状态闸：只有 `POSTED` 能冲，其余按「当前是哪一态」给下一步动作。
fn reverse_blocked(sheet: &SheetRow) -> ApiError {
    let hint = match sheet.status.as_str() {
        "DRAFT" | STATUS_CALCULATED => "：这张单还没有生成凭证，不需要红冲（直接重算或作废）",
        STATUS_SUBMITTED => "：审批中的单没有凭证，要改请等审批驳回或先撤回",
        STATUS_APPROVED => "：这张单还没过账（没有凭证可冲），直接重算/作废即可",
        STATUS_REVERSED => "：这张单已经红冲过了（终态），同期间可以重新试算新开一张",
        _ => "",
    };
    ApiError::new(
        409,
        error_code::HR_SALARY_RULE_BLOCKED,
        format!(
            "薪资单 {} 当前是「{}」（{}），只有已过账（POSTED）的单可以红冲{}",
            sheet.sheet_no,
            salary_dto::status_label(&sheet.status),
            sheet.status,
            hint
        ),
    )
}

// ==================== ④ 审批回调 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalaryVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（**不出凭证**，等人工过账）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → CALCULATED`（退回重算，可改可重提）。
    BackToCalculated,
}

impl SalaryVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SalaryVerdict::Approve => "APPROVE",
            SalaryVerdict::BackToCalculated => "BACK_TO_CALCULATED",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动薪资单）。
///
/// ⚠️ 白名单列终态（与 `leave_bridge::leave_verdict` 同款）：`hr_salary` 是
/// 「人事 → 财务会签 → 大额总经理」的多段流程，中间站批完时回调传来的是 `PENDING`，
/// 用「非 APPROVED 就打回」会把刚过人事、还在等财务会签的工资单直接退回重算。
pub fn salary_verdict(instance_status: &str) -> Option<SalaryVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(SalaryVerdict::Approve),
        "REJECTED" | "REJECTED_TERMINATED" | "WITHDRAWN" | "CANCELLED" | "TERMINATED" => {
            Some(SalaryVerdict::BackToCalculated)
        }
        _ => None,
    }
}

/// 审批单落终态 → 回写薪资单状态（`FlowCallback` 身份）。
///
/// 返回值：被推进的薪资单 id（`None` = 与本桥无关 / 中间态 / 失败已记日志）。
pub async fn on_salary_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let Some(verdict) = salary_verdict(instance_status) else {
        tracing::debug!(%instance_id, instance_status, "薪资审批仍在进行：不回写单据");
        return None;
    };
    match settle(instance_id, verdict).await {
        Ok(Some(sheet_id)) => {
            tracing::info!(
                %instance_id,
                %sheet_id,
                verdict = verdict.as_str(),
                instance_status,
                "薪资审批结论已回写单据"
            );
            Some(sheet_id)
        }
        Ok(None) => None,
        Err(error) => {
            // 通过失败用 ERROR：审批过了但单据还在「审批中」= 这个月工资发不出去
            // （过账要求 APPROVED）；打回失败用 WARN：单据仍是审批中，至少还有待办推它。
            if verdict == SalaryVerdict::Approve {
                tracing::error!(
                    %instance_id,
                    code = error.code,
                    msg = %error.msg,
                    "薪资审批通过但单据回写失败（审批单不回滚）：单据仍停在 SUBMITTED，需人工重试"
                );
            } else {
                tracing::warn!(
                    %instance_id,
                    code = error.code,
                    msg = %error.msg,
                    "薪资审批未通过但打回失败（审批单不回滚）"
                );
            }
            None
        }
    }
}

/// [`on_salary_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: SalaryVerdict) -> std::result::Result<Option<Uuid>, ApiError> {
    // ① 审批单必须是「薪资审批」类的（全量调用不按 type 分流，这里自己判）
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询薪资审批单"))?
    else {
        tracing::debug!(%instance_id, "审批单不存在，跳过薪资回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(sheet_id) = instance.biz_id else {
        return Ok(None);
    };
    let operator = approver_operator(&instance, instance_id).await?;
    let comment = latest_comment(instance_id)
        .await
        .map_err(|error| internal(error, "查询审批意见"))?;
    let (to_status, reason) = match verdict {
        SalaryVerdict::Approve => (STATUS_APPROVED, "薪资审批通过（凭证在过账时生成）".to_string()),
        SalaryVerdict::BackToCalculated => (
            STATUS_CALCULATED,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };

    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启薪资审批回写事务"))?;
    let outcome = async {
        // ② 锁单：并发下「HR 撤回」与「审批通过」只能有一个生效
        let Some(sheet) = model::lock_sheet(&tx, sheet_id)
            .await
            .map_err(|error| internal(error, "锁定薪资单"))?
        else {
            tracing::warn!(%sheet_id, "薪资单不存在或已删除，跳过审批回调");
            return Ok::<Option<Uuid>, ApiError>(None);
        };
        // ③ 状态闸：只有审批中能接受结论（重复回调 / 已被撤回 → 幂等跳过）
        if sheet.status != STATUS_SUBMITTED {
            tracing::info!(
                sheet_no = %sheet.sheet_no,
                status = %sheet.status,
                verdict = verdict.as_str(),
                "薪资单不在审批中（重复回调 / 已处理）：跳过"
            );
            return Ok(None);
        }
        // ④ 陈旧回调闸：这单已经挂在**另一张**实例上了（撤回后重提）→ 旧实例的结论不采纳
        if let Some(current) = sheet.approval_instance_id
            && current != instance_id
        {
            tracing::warn!(
                sheet_no = %sheet.sheet_no,
                %current,
                %instance_id,
                "薪资单已改挂新实例：忽略旧实例的审批结论"
            );
            return Ok(None);
        }
        // ⑤ 状态机（唯一入口：边校验 + 写 biz_status_log）
        state_machine::transition(
            &tx,
            ENTITY_SALARY_SHEET,
            sheet_id,
            &sheet.sheet_no,
            &state_machine::SALARY_SHEET_EDGES,
            STATUS_SUBMITTED,
            to_status,
            operator.clone(),
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ⑥ CAS 回写业务表
        let affected = model::set_status(&tx, sheet_id, STATUS_SUBMITTED, to_status, operator.id)
            .await
            .map_err(|error| write_error(error, "回写薪资单审批结论", &sheet.sheet_no))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("薪资单 {} 的状态在审批回调期间已被改动，请刷新后重试", sheet.sheet_no),
            ));
        }
        Ok(Some(sheet_id))
    }
    .await;
    match outcome {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| internal(error, "提交薪资审批回写事务"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 这一票的操作人：最后一票的审批人；读不到退回**发起人**（审计列宁可写发起人也不留空）。
async fn approver_operator(
    instance: &flow_model::InstanceRow,
    instance_id: Uuid,
) -> std::result::Result<Operator, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| internal(error, "查询审批人"))?;
    let approver: Option<Uuid> = match rows.first() {
        None => None,
        Some(row) => row.get_typed("assignee_id").ok().flatten(),
    };
    let Some(approver) = approver else {
        return Ok(Operator::named(
            Some(instance.initiator_id),
            instance.initiator_name.clone().unwrap_or_default(),
        ));
    };
    let name = flow_model::user_profile(approver)
        .await
        .map_err(|error| internal(error, "查询审批人姓名"))?
        .map(|(name, _dept)| name)
        .unwrap_or_default();
    Ok(Operator::named(Some(approver), name))
}

/// 最近一条**审批意见**（驳回时写进状态日志；通过时也存一份，审批人常写「同意，本月按此发放」）。
async fn latest_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
                AND coalesce(comment, '') <> '' \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok().flatten()),
    }
}

/// 自审批判据（纯函数，单测逐条钉死）：返回**拒绝理由**，`None` = 放行。
///
/// 薪资单的判据只有一条：`initiator_id == operator` —— 谁提交的谁不能批。
/// 设计 §4.3 第 2 条原文「工资单的发起人（HR）不能是财务经理那一签的同一人」。
pub fn self_approval_reason(initiator_id: Uuid, operator: Uuid) -> Option<&'static str> {
    if initiator_id == operator {
        return Some("薪资单的发起人不能审批自己提交的单据：请交给人事经理 / 财务 / 总经理处理");
    }
    None
}

/// 审批入口的内控闸（**事务之前**）：发起人不能批自己那一票 → 403 + 65019。
///
/// 与本业务无关的待办零影响：`biz_type != HR_SALARY` 直接放行。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| internal(error, "查询待办"))?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| internal(error, "查询审批单"))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    match self_approval_reason(instance.initiator_id, operator) {
        None => Ok(()),
        Some(reason) => Err(ApiError::new(403, error_code::FLOW_SELF_APPROVAL_FORBIDDEN, reason)),
    }
}

/// 终止薪资单上挂着的审批实例（撤回时用，在跑的才终止）。
///
/// 由 `salary_sheet_handler::withdraw_sheet` 在**同一事务**里调用：不终止的话，财务的
/// 待办里会留一张「已经回到重算」的单，点同意会推一个不存在的状态，并且重提时
/// 新实例会撞 `uk_biz_flow_instance_biz_active`（`PENDING` 部分唯一索引）。
pub async fn cancel_approval_of(tx: &Transaction, sheet: &SheetRow) -> std::result::Result<Option<String>, ApiError> {
    let Some(instance_id) = sheet.approval_instance_id else {
        return Ok(None);
    };
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询薪资审批实例"))?
    else {
        return Ok(None);
    };
    if !instance.is_pending() {
        return Ok(Some(instance.status));
    }
    let out = engine::terminate_instance(tx, instance_id, "CANCELLED", "薪资单撤回：审批终止")
        .await
        .map_err(|error| internal(error, "终止薪资审批实例"))?;
    if let Some(rejected) = out.rejected {
        return Err(rejected);
    }
    Ok(Some("CANCELLED".to_string()))
}

// ==================== ⑤ 错误原语 ====================

/// `404 + 10002`：单不存在（或已软删）。
pub fn not_found(sheet_id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("薪资单 {sheet_id} 不存在（可能已删除，或 id 抄错）"),
    )
}

/// 状态不对 → `409 + 68008`，报文点明当前状态与**下一步该点哪个按钮**。
fn status_blocked(sheet: &SheetRow) -> ApiError {
    let hint = match sheet.status.as_str() {
        STATUS_CALCULATED | "DRAFT" => "：请先「提交审批」",
        STATUS_SUBMITTED => "：审批中的单要等审批通过后才能过账（审批人驳回会退回重算）",
        STATUS_POSTED => "：这张单已经过账了（两张凭证都在），要重发请先红冲",
        STATUS_REVERSED => "：这张单已红冲（终态），要重发请新开一张（补差单）",
        _ => "",
    };
    ApiError::new(
        409,
        error_code::HR_SALARY_RULE_BLOCKED,
        format!(
            "薪资单 {} 当前是「{}」（{}），只有已审批通过（APPROVED）的单可以过账{}",
            sheet.sheet_no,
            salary_dto::status_label(&sheet.status),
            sheet.status,
            hint
        ),
    )
}

/// `DbError` → `ApiError`（内部错误）。
pub fn internal(error: DbError, ctx: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{ctx}失败：{error}"))
}

/// 凭证相关失败：科目缺失 / 非末级 → 400（报文原文已点名科目）；其余 → 500。
fn voucher_problem(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::InvalidValue(message) => {
            ApiError::new(400, error_code::PARAM_INVALID, format!("{ctx}失败：{message}"))
        }
        other => internal(other, ctx),
    }
}

/// 写路径失败：唯一键 / 外键等 → 409 带原文（与 `salary_sheet_model::write_error` 同口径）。
fn write_error(error: DbError, ctx: &str, sheet_no: &str) -> ApiError {
    model::write_error(error, ctx, sheet_no)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dept() -> Uuid {
        Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111)
    }

    fn other_dept() -> Uuid {
        Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222)
    }

    /// 带 DEPT 辅助核算的科目（`5602.1x` / `2211.0x`，与 V0384 种的维度一致）。
    fn aware() -> HashSet<String> {
        ["5602.10", "5602.11", "5602.12", "2211.01", "2211.02", "2211.03"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn item(code: &str, item_type: &str, debit: &str, credit: &str, amount: &str, dept_id: Option<Uuid>) -> ItemAmount {
        ItemAmount {
            item_code: code.to_string(),
            item_name: format!("{code}-名称"),
            item_type: item_type.to_string(),
            debit_code: Some(debit.to_string()),
            credit_code: Some(credit.to_string()),
            amount: amount.parse().expect("金额字面量"),
            dept_id,
        }
    }

    /// 设计 §4.6.1 的那套数字（张三：应发 9954.03 / 单位社保 1680 / 单位公积金 960）。
    fn design_items(d: Option<Uuid>) -> Vec<ItemAmount> {
        vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "9954.03", d),
            item("SI_COMPANY", ITEM_EMPLOYER, "5602.11", "2211.02", "1680.00", d),
            item("HF_COMPANY", ITEM_EMPLOYER, "5602.12", "2211.03", "960.00", d),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "840.00", d),
            item("HF_PERSONAL", ITEM_WITHHOLD, "", "2241.04", "960.00", d),
            item("IIT", ITEM_WITHHOLD, "", "2221.03", "0", d),
        ]
    }

    fn find<'a>(lines: &'a [PostLine], code: &str, dept_id: Option<Uuid>) -> &'a PostLine {
        lines
            .iter()
            .find(|line| line.account_code == code && line.aux_dept_id == dept_id)
            .unwrap_or_else(|| panic!("没有科目 {code} / 部门 {dept_id:?} 的行：{lines:#?}"))
    }

    /// ① 计提凭证与设计 §4.6.1 ① 逐行对账（借贷各 12594.03，六行）。
    #[test]
    fn accrual_matches_design_table() {
        let lines = plan_accrual(&design_items(Some(dept())), &aware()).expect("计提汇总必须成功");
        assert_eq!(lines.len(), 6, "三借三贷：{lines:#?}");
        assert_eq!(find(&lines, "5602.10", Some(dept())).debit, "9954.03".parse().unwrap());
        assert_eq!(find(&lines, "5602.11", Some(dept())).debit, "1680.00".parse().unwrap());
        assert_eq!(find(&lines, "5602.12", Some(dept())).debit, "960.00".parse().unwrap());
        assert_eq!(find(&lines, "2211.01", Some(dept())).credit, "9954.03".parse().unwrap());
        assert_eq!(find(&lines, "2211.02", Some(dept())).credit, "1680.00".parse().unwrap());
        assert_eq!(find(&lines, "2211.03", Some(dept())).credit, "960.00".parse().unwrap());
        let debit: Decimal = lines.iter().map(|line| line.debit).sum();
        let credit: Decimal = lines.iter().map(|line| line.credit).sum();
        assert_eq!(debit, "12594.03".parse::<Decimal>().unwrap());
        assert_eq!(debit, credit, "计提凭证必须借贷平衡");
    }

    /// ② 发放凭证与设计 §4.6.1 ② 逐行对账（个税 0 → **不落行**）。
    #[test]
    fn payment_matches_design_table_and_drops_zero_lines() {
        let lines = plan_payment(&design_items(Some(dept())), &aware(), "1002").expect("发放汇总必须成功");
        // 借 2211.01 / 贷 1002 + 2241.03 + 2241.04 = 4 行（个税 0 不落）
        assert_eq!(lines.len(), 4, "个税为 0 不许落行：{lines:#?}");
        assert_eq!(find(&lines, "2211.01", Some(dept())).debit, "9954.03".parse().unwrap());
        assert_eq!(find(&lines, "1002", None).credit, "8154.03".parse().unwrap());
        assert_eq!(find(&lines, "2241.03", None).credit, "840.00".parse().unwrap());
        assert_eq!(find(&lines, "2241.04", None).credit, "960.00".parse().unwrap());
        let debit: Decimal = lines.iter().map(|line| line.debit).sum();
        let credit: Decimal = lines.iter().map(|line| line.credit).sum();
        assert_eq!(debit, "9954.03".parse::<Decimal>().unwrap());
        assert_eq!(debit, credit, "发放凭证必须借贷平衡");
        // 代扣科目没有 DEPT 维度 → 不带部门（带了会让辅助账多一层切分）
        assert!(
            lines
                .iter()
                .all(|line| line.account_code == "2211.01" || line.aux_dept_id.is_none())
        );
    }

    /// ③ **净额法**：收入 8200 − 考勤扣款 735.63 → 一行 7464.37（不是 8200 + 负数行）。
    ///
    /// 这是本卡的核心验收项：单边 `CHECK` 要求每行金额 > 0，所以扣款必须抵减同一行的借方。
    #[test]
    fn pre_deduct_is_netted_into_the_same_expense_line() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "8000", Some(dept())),
            item("ATT_BONUS", ITEM_EARNING, "5602.10", "2211.01", "200", Some(dept())),
            item(
                "ATT_DEDUCT",
                ITEM_PRE_DEDUCT,
                "5602.10",
                "2211.01",
                "735.63",
                Some(dept()),
            ),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "840", Some(dept())),
        ];
        let accrual = plan_accrual(&items, &aware()).expect("净额法必须成功");
        // 只有 2 行：借 5602.10 7464.37 / 贷 2211.01 7464.37
        assert_eq!(accrual.len(), 2, "{accrual:#?}");
        assert_eq!(
            find(&accrual, "5602.10", Some(dept())).debit,
            "7464.37".parse().unwrap()
        );
        assert_eq!(
            find(&accrual, "2211.01", Some(dept())).credit,
            "7464.37".parse().unwrap()
        );
        // 一行都没有负数（单边 CHECK 的硬要求）
        assert!(
            accrual
                .iter()
                .all(|line| line.debit >= Decimal::ZERO && line.credit >= Decimal::ZERO)
        );

        let payment = plan_payment(&items, &aware(), "1002").expect("发放汇总必须成功");
        assert_eq!(
            find(&payment, "2211.01", Some(dept())).debit,
            "7464.37".parse().unwrap()
        );
        assert_eq!(find(&payment, "1002", None).credit, "6624.37".parse().unwrap());
        assert_eq!(find(&payment, "2241.03", None).credit, "840".parse().unwrap());
    }

    /// ④ 净额为 0 的组不落行（部门里只有扣款没有收入 → 净额 0 → 该部门整组消失）。
    #[test]
    fn zero_net_group_is_dropped() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "1000", Some(dept())),
            item(
                "ATT_DEDUCT",
                ITEM_PRE_DEDUCT,
                "5602.10",
                "2211.01",
                "1000",
                Some(dept()),
            ),
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "5000", Some(other_dept())),
        ];
        let accrual = plan_accrual(&items, &aware()).expect("净额 0 的组要丢掉，不是报错");
        // 只有另一个部门的两行
        assert_eq!(accrual.len(), 2, "{accrual:#?}");
        assert_eq!(
            find(&accrual, "5602.10", Some(other_dept())).debit,
            "5000".parse().unwrap()
        );
        assert!(
            accrual.iter().all(|line| line.aux_dept_id != Some(dept())),
            "净额 0 的部门不许落行（否则撞单边 CHECK）"
        );
    }

    /// ⑤ 净额为负 → 拒绝过账，且报文点名科目 + 部门（不是静默给 0）。
    #[test]
    fn negative_net_is_rejected_with_department_in_message() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "100", Some(dept())),
            item(
                "ATT_DEDUCT",
                ITEM_PRE_DEDUCT,
                "5602.10",
                "2211.01",
                "735.63",
                Some(dept()),
            ),
        ];
        let error = plan_accrual(&items, &aware()).expect_err("净额为负必须拒绝");
        match &error {
            PostPlanError::NegativeNet { net, .. } => {
                assert_eq!(*net, "-635.63".parse::<Decimal>().unwrap());
            }
            other => panic!("应报净额为负，实际 {other:?}"),
        }
        let message = error.message();
        assert!(message.contains("5602.10"), "{message}");
        assert!(message.contains(&dept().to_string()), "报文要点名部门：{message}");
    }

    /// ⑥ 部门辅助核算：同一科目按部门拆行；**不带 DEPT 维度的科目合并不拆**
    ///    （`dept_aware` 里没有 `2241.03`，所以两个部门的社保个人合成一行）。
    #[test]
    fn dept_split_only_for_dept_aware_accounts() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "3000", Some(dept())),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "100", Some(dept())),
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "5000", Some(other_dept())),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "200", Some(other_dept())),
        ];
        let accrual = plan_accrual(&items, &aware()).expect("计提");
        assert_eq!(find(&accrual, "5602.10", Some(dept())).debit, "3000".parse().unwrap());
        assert_eq!(
            find(&accrual, "5602.10", Some(other_dept())).debit,
            "5000".parse().unwrap()
        );
        assert_eq!(
            find(&accrual, "2211.01", Some(other_dept())).credit,
            "5000".parse().unwrap()
        );

        let payment = plan_payment(&items, &aware(), "1002").expect("发放");
        // 社保个人两个部门合并成一行 300（2241.03 没有 DEPT 维度）
        assert_eq!(find(&payment, "2241.03", None).credit, "300".parse().unwrap());
        // 银行存款 = 8000 − 300
        assert_eq!(find(&payment, "1002", None).credit, "7700".parse().unwrap());
        // 应付职工薪酬按部门拆（2211.01 有 DEPT 维度）
        assert_eq!(find(&payment, "2211.01", Some(dept())).debit, "3000".parse().unwrap());
        assert_eq!(
            find(&payment, "2211.01", Some(other_dept())).debit,
            "5000".parse().unwrap()
        );
    }

    /// ⑦ `amount = 0` 的项一个组都不建（「全勤奖 0」只在 `detail_item` 里有意义）。
    #[test]
    fn zero_amount_items_produce_no_lines() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "8000", Some(dept())),
            item("ATT_BONUS", ITEM_EARNING, "5602.10", "2211.01", "0", Some(dept())),
            item("ATT_DEDUCT", ITEM_PRE_DEDUCT, "5602.10", "2211.01", "0", Some(dept())),
        ];
        let accrual = plan_accrual(&items, &aware()).expect("0 金额项跳过");
        assert_eq!(accrual.len(), 2, "{accrual:#?}");
        assert_eq!(find(&accrual, "5602.10", Some(dept())).debit, "8000".parse().unwrap());
    }

    /// ⑧ 金额全为 0 → `NoLines`（这种单不该出凭证，报文说清「不需要过账」）。
    #[test]
    fn all_zero_sheet_has_no_voucher() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "0", Some(dept())),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "0", Some(dept())),
        ];
        assert_eq!(plan_accrual(&items, &aware()), Err(PostPlanError::NoLines));
        assert_eq!(
            plan_payment(&items, &aware(), "1002"),
            Err(PostPlanError::NoLines),
            "应发 0 的单没有可发放的工资"
        );
    }

    /// ⑨ 代扣合计 > 应发 → 倒挤的银行存款为负 → 拒绝（不是把银行写成 0 糊过去）。
    #[test]
    fn withhold_over_gross_is_rejected() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "1000", Some(dept())),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "1200", Some(dept())),
        ];
        let error = plan_payment(&items, &aware(), "1002").expect_err("代扣超过应发必须拒绝");
        match &error {
            PostPlanError::NegativeBank { amount } => {
                assert_eq!(*amount, "-200".parse::<Decimal>().unwrap());
            }
            other => panic!("应报银行存款为负，实际 {other:?}"),
        }
    }

    /// ⑩ 代扣刚好等于应发 → 不落银行行（实发 0，仍然是一张合法凭证）。
    #[test]
    fn zero_bank_line_is_dropped_but_voucher_stays_balanced() {
        let items = vec![
            item("BASE", ITEM_EARNING, "5602.10", "2211.01", "1000", Some(dept())),
            item("SI_PERSONAL", ITEM_WITHHOLD, "", "2241.03", "1000", Some(dept())),
        ];
        let payment = plan_payment(&items, &aware(), "1002").expect("实发 0 是合法的");
        assert_eq!(payment.len(), 2, "{payment:#?}");
        assert!(payment.iter().all(|line| line.account_code != "1002"));
        let debit: Decimal = payment.iter().map(|line| line.debit).sum();
        let credit: Decimal = payment.iter().map(|line| line.credit).sum();
        assert_eq!(debit, credit);
    }

    /// ⑪ 缺科目 → `MissingAccount` 点名声薪项编码（配置页改一半就过账的典型错）。
    #[test]
    fn missing_account_names_the_item() {
        let mut broken = item("BASE", ITEM_EARNING, "5602.10", "2211.01", "1000", Some(dept()));
        broken.debit_code = None;
        let error = plan_accrual(&[broken], &aware()).expect_err("缺借方科目必须报错");
        assert_eq!(
            error,
            PostPlanError::MissingAccount {
                item_code: "BASE".to_string(),
                side: "debit"
            }
        );
        assert!(error.message().contains("BASE"));
    }

    /// ⑫ 审批结论映射：通过 / 驳回 / 中间态三条路径（中间态**必须**不动单据）。
    #[test]
    fn verdict_maps_three_paths() {
        assert_eq!(salary_verdict("APPROVED"), Some(SalaryVerdict::Approve));
        assert_eq!(salary_verdict(" approved "), Some(SalaryVerdict::Approve));
        for status in [
            "REJECTED",
            "REJECTED_TERMINATED",
            "rejected_terminated",
            "WITHDRAWN",
            "CANCELLED",
            "TERMINATED",
        ] {
            assert_eq!(
                salary_verdict(status),
                Some(SalaryVerdict::BackToCalculated),
                "{status} 必须退回重算（不是关闭、不是作废）"
            );
        }
        // 财务会签只有一站批完时，引擎回调传来的是 PENDING —— 不许把单打回
        for status in ["PENDING", "", "STARTED", "APPROVED2", "DRAFT"] {
            assert_eq!(salary_verdict(status), None, "{status} 不该回写薪资单");
        }
    }

    /// ⑬ 自审批守卫：发起人自己批 → 拦；别人批 → 放行。
    #[test]
    fn self_approval_blocks_initiator_only() {
        let initiator = Uuid::now_v7();
        let other = Uuid::now_v7();
        assert!(self_approval_reason(initiator, initiator).is_some());
        assert!(self_approval_reason(initiator, other).is_none());
    }

    /// ⑭ 常量与种子 / 词表同口径（错一个字就查不到流程 / 巡检判红）。
    #[test]
    fn constants_match_seed_and_vocabulary() {
        assert_eq!(BIZ_TYPE, "HR_SALARY");
        assert_eq!(FLOW_CODE, "hr_salary");
        assert_eq!(SOURCE_ACCRUAL, "PAYROLL_ACCRUAL");
        assert_eq!(SOURCE_PAYMENT, "PAYROLL_PAYMENT");
        // 词表里必须有这两个来源（HR-06 扩过；缺了巡检 biz_type_domain.py 会判红）
        assert!(VoucherSourceType::PayrollAccrual.doc_kind().is_some());
        assert!(VoucherSourceType::PayrollPayment.doc_kind().is_some());
        assert_eq!(VoucherSourceType::PayrollAccrual.as_str(), SOURCE_ACCRUAL);
    }

    /// ⑮ 边表与本桥要写的两个状态都在状态域里（防 `salary_sheet_dto` 改字面量后静默写错）。
    #[test]
    fn edges_and_statuses_are_registered() {
        let edges = state_machine::SALARY_SHEET_EDGES;
        assert!(edges.contains(&(STATUS_SUBMITTED, STATUS_APPROVED)));
        assert!(edges.contains(&(STATUS_SUBMITTED, STATUS_CALCULATED)));
        assert!(edges.contains(&(STATUS_APPROVED, STATUS_POSTED)));
        assert_eq!(
            edges.as_slice(),
            state_machine::Entity::SalarySheet.edges(),
            "本桥用的边表必须与状态机注册表完全一致"
        );
        assert!(salary_dto::STATUSES.iter().any(|(code, _)| *code == STATUS_POSTED));
    }

    // ==================== 红冲（HR-40） ====================

    /// 一张单头（只填红冲相关的列；金额列给 0 —— 红冲不读它们）。
    fn sheet_with_status(status: &str) -> SheetRow {
        SheetRow {
            id: Uuid::now_v7(),
            sheet_no: "SAL-202611-000001".to_string(),
            period_code: "2026-11".to_string(),
            period_start: "2026-11-01".parse().expect("日期"),
            period_end: "2026-11-30".parse().expect("日期"),
            status: status.to_string(),
            attendance_period_id: None,
            employee_count: 0,
            earning_total: Decimal::ZERO,
            pre_deduct_total: Decimal::ZERO,
            gross_total: Decimal::ZERO,
            withhold_total: Decimal::ZERO,
            net_total: Decimal::ZERO,
            si_company_total: Decimal::ZERO,
            hf_company_total: Decimal::ZERO,
            accrual_voucher_id: None,
            accrual_voucher_no: None,
            payment_voucher_id: None,
            payment_voucher_no: None,
            reversal_voucher_id: None,
            approval_instance_id: None,
            calculated_at: None,
            calculated_by: None,
            submitted_at: None,
            posted_at: None,
            reversed_at: None,
            reverse_reason: None,
            remark: None,
            created_at: None,
            updated_at: None,
        }
    }

    /// ⑯ 红冲的边在状态机里，且**要求原因**（`POSTED → REVERSED` 是必填原因边）。
    ///
    /// 这条是「原因必填」的**第二道**钉子：DTO 层拦空原因，状态机层再拦一次
    /// （绕过 handler 的内部调用也得守）。少任何一道，都会有一条路径能无声无息冲账。
    #[test]
    fn reverse_edge_requires_reason() {
        let reason_edges = state_machine::Entity::SalarySheet.reason_edges();
        assert!(
            reason_edges.contains(&(STATUS_POSTED, STATUS_REVERSED)),
            "POSTED → REVERSED 必须是必填原因边，实际：{reason_edges:?}"
        );
        // 反向：过账（APPROVED → POSTED）不该强制原因（否则「过账」按钮要用户先写 50 字）
        assert!(!reason_edges.contains(&(STATUS_APPROVED, STATUS_POSTED)));
        // 终态：REVERSED 不能再被改（明细覆盖 / 重算都走 ensure_writable）
        assert!(
            state_machine::Entity::SalarySheet
                .locked_statuses()
                .contains(&STATUS_REVERSED),
            "REVERSED 必须是冻结态（终态）"
        );
    }

    /// ⑰ 红冲的状态闸：只有 POSTED 放行，其余每一态都给**下一步动作**（不是干巴巴的 409）。
    #[test]
    fn reverse_blocked_explains_the_next_step_per_status() {
        // 放行的那一态：闸函数本身不该被调用（调用点先比对状态），但表也不能是空的
        for (status, expect) in [
            (salary_dto::STATUS_DRAFT, "直接重算或作废"),
            (STATUS_CALCULATED, "直接重算或作废"),
            (STATUS_SUBMITTED, "审批中"),
            (STATUS_APPROVED, "还没过账"),
            (STATUS_REVERSED, "已经红冲"),
        ] {
            let sheet = sheet_with_status(status);
            let error = reverse_blocked(&sheet);
            assert_eq!((error.status, error.code), (409, error_code::HR_SALARY_RULE_BLOCKED));
            assert!(
                error.msg.contains("SAL-202611-000001"),
                "{status} 的报文必须点名单号：{}",
                error.msg
            );
            assert!(
                error.msg.contains("POSTED"),
                "{status} 的报文要点名目标状态：{}",
                error.msg
            );
            assert!(
                error.msg.contains(expect),
                "{status} 的报文要给出下一步（期望包含「{expect}」）：{}",
                error.msg
            );
        }
    }

    /// ⑱ 空白原因在 bridge 入口就 400（不等开事务）—— 本卡验收项的最后一道自守。
    ///
    /// 这条用例**不连库**：`reverse_salary` 先判原因再 `db().begin()`，
    /// 所以没有库地址时也能跑（CI 的 `cargo test --lib` 会跑它）。
    #[tokio::test]
    async fn blank_reason_is_rejected_before_touching_the_db() {
        let operator = Operator::named(Some(Uuid::now_v7()), "hr40-test");
        let sheet_id = Uuid::now_v7();
        for blank in ["", "   ", "\t\n "] {
            let error = reverse_salary(operator.clone(), sheet_id, blank)
                .await
                .expect_err("空白原因必须拒绝");
            assert_eq!((error.status, error.code), (400, error_code::PARAM_INVALID));
            assert!(error.msg.contains("reason 必填"), "报文：{}", error.msg);
            assert!(!error.msg.contains("SELECT"), "报文不许是 SQL 原文：{}", error.msg);
        }
    }
}
