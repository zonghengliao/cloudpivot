//! 费用报销的**纯计算**（ARCH-6 第四章 §4.5）：价税拆分 / 汇总 / 凭证行 / 分级 / 期间。
//!
//! # 为什么先写纯函数
//!
//! ARCH-6 §4.5.2 的算例（一单 4 行明细、含「不可抵扣类型混在可抵扣单据里」）
//! 是本卡最容易算错的地方：把「税额 = 含税 / (1+税率) × 税率」无差别套到每一行，
//! 借贷**看起来是平的**（6,600 = 6,600），但招待费的 60 元进项被算进了 `2221.01`
//! （向税局多申报留抵），同时费用少记 60 元 —— 借贷平衡掩盖了账错。
//!
//! 所以本模块只做算术，不碰库：每条明细怎么拆、按什么口径汇总、余额怎么判，
//! 全部在这里单测钉死；`expense_bridge` / `expense_handler` 只负责编排事务。
//!
//! # 四个口径（与设计逐条对应）
//!
//! ```text
//! ① 明细：with_tax = amount + tax                        （库级 ck_biz_exp_item_tax）
//! ② 税额：tax = tax_deductible ? round(with_tax/(1+rate) × rate, 4) : 0
//!        amount = with_tax − tax                         （不可抵扣 → 全额进费用）
//! ③ 单头：total_with_tax = total_amount + total_tax      （库级 ck_biz_exp_report_tax_sum）
//! ④ 凭证：借方按 (费用科目) 聚合 amount ＋ 税行 2221.01 = Σ tax；贷方 2241.01 = total_with_tax
//! ```

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;

/// 金额小数位（与 `NUMERIC(18,4)` 一致）。
pub const MONEY_SCALE: u32 = 4;

/// 金额归一到 4 位小数（四舍五入）。所有落库金额都必须过它一次（列是 `NUMERIC(18,4)`）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 金额归一到「分」（2 位小数，四舍五入）。
///
/// 价税拆分用**分**而不是 4 位：中国会计与增值税申报的最小单位都是分，
/// 多留两位小数会让「税额 = 27.6106」这种数进凭证与申报表（对不上税务口径）。
/// 与设计 §4.5.2 的算例口径一致（212.39 / 27.61）；设计正文写的是 `round(…, 4)`，
/// 但它自己的算例是分 —— 以算例为准，差异写进交付说明。
pub fn cent(value: Decimal) -> Decimal {
    value.round_dp(2)
}

/// 一条明细的**价税拆分结果**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    /// 不含税金额（进费用科目的金额）。
    pub amount: Decimal,
    /// 税额（可抵扣时进 `2221.01`；不可抵扣时为 0 并并入 `amount`）。
    pub tax: Decimal,
    /// 价税合计（= `amount + tax`，恒等于用户录入的含税金额）。
    pub with_tax: Decimal,
}

/// 拆一行明细的价税。
///
/// * `with_tax`：用户录入的**含税**金额（前端「含税录入 → 自动拆」，ARCH-6 §4.10 细节 1）；
/// * `rate`：税率（0~1，0 表示不开票 / 免税）；
/// * `deductible`：该费用类型的进项税是否可抵扣（`biz_expense_type.tax_deductible`）。
///
/// **不可抵扣 → 税额恒 0、金额 = 含税金额**（ARCH-6 §4.5.3 硬规定：
/// 招待费 / 福利费 / 罚款的进项不得抵扣，系统不处理就必然错）。
pub fn split(with_tax: Decimal, rate: Decimal, deductible: bool) -> Split {
    let with_tax = money(with_tax);
    if !deductible || rate <= Decimal::ZERO {
        return Split {
            amount: with_tax,
            tax: Decimal::ZERO,
            with_tax,
        };
    }
    // 先算不含税金额（四舍五入到分），税额取差额 ⇒ `amount + tax == with_tax` **恒成立**
    // （两次独立舍入会让「分」凭空多一分或少一分，那是月末对不上账的经典成因）。
    let base = cent(with_tax / (Decimal::ONE + rate));
    let tax = money(with_tax - base);
    Split {
        amount: money(with_tax - tax),
        tax,
        with_tax,
    }
}

/// 一张报销单的汇总（三件套 + 条数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// 不含税合计（= Σ 明细 `amount`）。
    pub total_amount: Decimal,
    /// 税额合计（= Σ 明细 `tax`）。
    pub total_tax: Decimal,
    /// 价税合计（= Σ 明细 `with_tax`）。
    pub total_with_tax: Decimal,
    /// 明细条数。
    pub item_count: i32,
}

impl Summary {
    /// 汇总若干条明细拆分结果。
    pub fn of(splits: &[Split]) -> Summary {
        let mut summary = Summary::default();
        for split in splits {
            summary.total_amount += split.amount;
            summary.total_tax += split.tax;
            summary.total_with_tax += split.with_tax;
            summary.item_count += 1;
        }
        summary.total_amount = money(summary.total_amount);
        summary.total_tax = money(summary.total_tax);
        summary.total_with_tax = money(summary.total_with_tax);
        summary
    }

    /// 单头三道恒等式的**自检**（写库前的最后一道；对不上就是 422 而非静默入库）。
    ///
    /// 返回 `Err(说明)` 时调用方给 `ERP_EXPENSE_TOTAL_MISMATCH`。
    pub fn check(&self) -> std::result::Result<(), String> {
        if self.item_count < 1 {
            return Err("报销单至少要一条明细（没有明细的报销单在会计上无意义）".to_string());
        }
        if self.total_with_tax != self.total_amount + self.total_tax {
            return Err(format!(
                "单头金额不自洽：价税合计 {} ≠ 不含税 {} + 税额 {}",
                self.total_with_tax, self.total_amount, self.total_tax
            ));
        }
        Ok(())
    }
}

/// 金额分级档位（ARCH-6 §4.4.1 的「直属上级 + 三档」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// `< 1000`：只走直属上级。
    UnderThousand,
    /// `1000 ≤ x < 10000`：上级 + 财务经理。
    UnderTenThousand,
    /// `≥ 10000`：上级 + 财务经理 + 总经理会签。
    TenThousandAndAbove,
}

/// 分级的两条阈值（**与 V0243 流程种子里的条件边逐字一致**，改一处必须改两处）。
pub const TIER_ONE: i64 = 1000;
/// 第二档阈值。
pub const TIER_TWO: i64 = 10000;

/// 按**含税金额**判档（`form.amount` = `total_with_tax`，与预算占用、`po_approve` 同口径）。
///
/// 边界归属：恰好 1000 归第二档、恰好 10000 归第三档（**宁可多一道审批**）。
pub fn tier_of(total_with_tax: Decimal) -> Tier {
    let threshold_one = Decimal::from(TIER_ONE);
    let threshold_two = Decimal::from(TIER_TWO);
    if total_with_tax < threshold_one {
        Tier::UnderThousand
    } else if total_with_tax < threshold_two {
        Tier::UnderTenThousand
    } else {
        Tier::TenThousandAndAbove
    }
}

impl Tier {
    /// 该档位需要的审批节点（中文，日志 / 报文用；**流程引擎才是真相**，这里只做展示）。
    pub const fn label(self) -> &'static str {
        match self {
            Tier::UnderThousand => "直属上级",
            Tier::UnderTenThousand => "直属上级 + 财务经理",
            Tier::TenThousandAndAbove => "直属上级 + 财务经理 + 总经理会签",
        }
    }
}

/// 凭证的一条借方（同一费用科目聚合后 = 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebitLine {
    /// 费用科目 id（明细里快照的 `account_id`）。
    pub account_id: Uuid,
    /// 借方金额（不含税，聚合后）。
    pub amount: Decimal,
}

/// 把明细按费用科目聚合（ARCH-6 §4.5.2「凭证借方按 (account_id) 聚合 amount」）。
///
/// 聚合顺序 = **科目首次出现的顺序**（不是哈希序）：同一张单两次生成的凭证行序一致，
/// 对账时人眼可逐行比对；零金额行直接丢弃（`post_multi_debit` 会拒 0 行）。
pub fn debit_lines(items: &[(Uuid, Decimal)]) -> Vec<DebitLine> {
    let mut out: Vec<DebitLine> = Vec::with_capacity(items.len());
    for (account_id, amount) in items {
        let amount = money(*amount);
        if amount <= Decimal::ZERO {
            continue;
        }
        match out.iter_mut().find(|line| line.account_id == *account_id) {
            Some(line) => line.amount = money(line.amount + amount),
            None => out.push(DebitLine {
                account_id: *account_id,
                amount,
            }),
        }
    }
    out
}

/// 付款状态（派生列 `pay_status`，库侧 `ck_biz_exp_report_pay_match` 保证与金额自洽）。
pub fn pay_status_of(total_with_tax: Decimal, paid: Decimal) -> &'static str {
    let total = money(total_with_tax);
    let paid = money(paid);
    if paid <= Decimal::ZERO {
        "UNPAID"
    } else if paid < total {
        "PARTIAL"
    } else {
        "PAID"
    }
}

/// 会计期间（`yyyy-MM`）。**入账期间 = 审批通过日所属期间**（ARCH-6 §4.5.4），
/// 不是费用发生日 —— 后者留在明细里供分析。
pub fn period_of(date: NaiveDate) -> String {
    date.format("%Y-%m").to_string()
}

/// **费用归属期间**：费用发生日所属月份（E2E06-FIX-3 ②）。
///
/// 预算占用按它归集（提交时 `biz_budget_usage.period_code` 取的就是建单时写入的
/// `period_of(expense_date)`，见 `expense_handler` 建单路径），与
/// [`accounting_period_code`] 的记账期间是**两个口径**。
///
/// 单独抽成函数而不是直接调 [`period_of`]，是为了让「哪个期间是费用期间、哪个是记账期间」
/// 在代码里有一处唯一表述（报表 / 详情 / 测试都引用它，避免有人把两个期间调反）。
pub fn expense_period_code(expense_date: NaiveDate) -> String {
    period_of(expense_date)
}

/// **记账期间**（凭证期间）：入账后才存在（`biz_expense_report.period_code` 在审批通过时
/// 被重算成审批日所属月份），未入账返回 `None`。
///
/// 传 `has_voucher` 而不是 `voucher_id`：调用方只需要「有没有计提凭证」这个事实，
/// 函数本身不关心凭证 id（纯函数才好测）。
pub fn accounting_period_code(has_voucher: bool, period_code: &str) -> Option<String> {
    has_voucher.then(|| period_code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量非法")
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("日期非法")
    }

    /// ① ARCH-6 §4.5.2 的算例：不可抵扣类型混在可抵扣单据里，逐分对上。
    ///
    /// 差旅 3,180（6% 可抵扣）+ 住宿 2,120（6% 可抵扣）+ 招待 1,060（6% **不可抵扣**）
    /// + 办公 240（13% 可抵扣）= 含税 6,600。
    #[test]
    fn design_example_6600_splits_exactly() {
        let travel = split(dec("3180"), dec("0.06"), true);
        let hotel = split(dec("2120"), dec("0.06"), true);
        let meal = split(dec("1060"), dec("0.06"), false);
        let office = split(dec("240"), dec("0.13"), true);

        assert_eq!((travel.amount, travel.tax), (dec("3000"), dec("180")));
        assert_eq!((hotel.amount, hotel.tax), (dec("2000"), dec("120")));
        // 不可抵扣：税额 0，60 元税并入费用（不是「借 2221.01 再转出」的两步做法）
        assert_eq!((meal.amount, meal.tax), (dec("1060"), dec("0")));
        assert_eq!((office.amount, office.tax), (dec("212.39"), dec("27.61")));

        let summary = Summary::of(&[travel, hotel, meal, office]);
        assert_eq!(summary.total_with_tax, dec("6600"));
        assert_eq!(summary.total_tax, dec("327.61"), "税行 = 180 + 120 + 27.61");
        assert_eq!(summary.total_amount, dec("6272.39"));
        assert_eq!(summary.item_count, 4);
        summary.check().expect("单头三道恒等式必须自洽");
    }

    /// ② 凭证借方按科目聚合：差旅与住宿同属 5602.02 → 一行 5,000（设计样例表格
    /// 第 1 行印成 3,000 是笔误，见交付说明的「实施差异」）。
    #[test]
    fn debit_lines_aggregate_by_account() {
        let travel_account = Uuid::from_u128(0x0056_0202);
        let meal_account = Uuid::from_u128(0x0056_0204);
        let office_account = Uuid::from_u128(0x0056_0203);
        let lines = debit_lines(&[
            (travel_account, dec("3000")),
            (travel_account, dec("2000")),
            (meal_account, dec("1060")),
            (office_account, dec("212.39")),
        ]);
        assert_eq!(lines.len(), 3, "两个科目不同的行不能合并");
        assert_eq!(lines[0].account_id, travel_account);
        assert_eq!(lines[0].amount, dec("5000"));
        assert_eq!(lines[1].amount, dec("1060"));
        assert_eq!(lines[2].amount, dec("212.39"));
        let debit_total: Decimal = lines.iter().map(|line| line.amount).sum();
        assert_eq!(
            debit_total + dec("327.61"),
            dec("6600"),
            "借方（费用 + 税）= 贷方（员工垫付）"
        );
    }

    /// ③ 零金额行必须丢弃（`post_multi_debit` 拒 0 行；否则凭证虚增行数）。
    #[test]
    fn debit_lines_drop_zero_amounts() {
        let account = Uuid::from_u128(1);
        let lines = debit_lines(&[(account, Decimal::ZERO), (account, dec("12.5"))]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].amount, dec("12.5"));
    }

    /// ④ 金额分级：边界归高档（1000 / 10000 都要多一道审批）。
    #[test]
    fn tier_boundaries_go_to_stricter_side() {
        assert_eq!(tier_of(dec("999.99")), Tier::UnderThousand);
        assert_eq!(tier_of(dec("1000")), Tier::UnderTenThousand);
        assert_eq!(tier_of(dec("9999.99")), Tier::UnderTenThousand);
        assert_eq!(tier_of(dec("10000")), Tier::TenThousandAndAbove);
        assert_eq!(tier_of(dec("0.01")), Tier::UnderThousand);
    }

    /// ⑤ 负金额 / 空明细：汇总层就要能识别（handler 再翻成 400/422）。
    #[test]
    fn summary_rejects_empty_items() {
        let summary = Summary::of(&[]);
        assert_eq!(summary.item_count, 0);
        assert!(summary.check().is_err(), "没有明细的报销单必须被拒");
    }

    /// ⑥ 单一明细、零税率（不开票）：税额 0，金额 = 含税金额。
    #[test]
    fn zero_rate_keeps_amount_equal_with_tax() {
        let line = split(dec("100"), Decimal::ZERO, true);
        assert_eq!(line.tax, Decimal::ZERO);
        assert_eq!(line.amount, dec("100"));
        assert_eq!(line.amount + line.tax, line.with_tax);
    }

    /// ⑦ 舍入不产生「分」的裂缝：amount + tax 恒等于录入的含税金额。
    #[test]
    fn rounding_never_breaks_identity() {
        for (with_tax, rate) in [
            ("0.01", "0.13"),
            ("212.39", "0.13"),
            ("333.33", "0.06"),
            ("9999.99", "0.09"),
            ("1000000.005", "0.06"),
        ] {
            let line = split(dec(with_tax), dec(rate), true);
            assert_eq!(
                line.amount + line.tax,
                money(dec(with_tax)),
                "含税 {with_tax} 税率 {rate} 拆分后合计必须还原"
            );
        }
    }

    /// ⑧ 付款状态三值：0 未付 / 部分 / 付清。
    #[test]
    fn pay_status_three_values() {
        let total = dec("600");
        assert_eq!(pay_status_of(total, Decimal::ZERO), "UNPAID");
        assert_eq!(pay_status_of(total, dec("200")), "PARTIAL");
        assert_eq!(pay_status_of(total, dec("600")), "PAID");
    }

    /// ⑨ 入账期间按日期所属月份（审批通过日），与费用发生日无关。
    #[test]
    fn period_follows_date() {
        assert_eq!(period_of(date(2026, 10, 31)), "2026-10");
        assert_eq!(period_of(date(2026, 11, 2)), "2026-11");
        assert_eq!(period_of(date(2026, 1, 1)), "2026-01");
    }

    /// ⑩ 两个期间是两个事实（E2E06-FIX-3 ②）：费用归属期间按费用发生日，
    /// 记账期间按审批通过日、且入账后才存在。
    ///
    /// 现场算例：2027-02 的费用在 2026-11 审批 → 凭证记 2026-11、预算占用记 2027-02。
    /// 这两个值必须能同时取到，界面才讲得清「为什么凭证在 11 月、占用在 2 月」。
    #[test]
    fn expense_and_accounting_periods_stay_separate() {
        let expense_date = date(2027, 2, 18);
        let approval_date = date(2026, 11, 9);

        assert_eq!(expense_period_code(expense_date), "2027-02");
        assert_eq!(period_of(approval_date), "2026-11");

        // 未入账：有费用期间，没有记账期间
        assert_eq!(accounting_period_code(false, &period_of(approval_date)), None);
        // 已入账：记账期间 = 审批日所属月，**不是**费用期间
        assert_eq!(
            accounting_period_code(true, &period_of(approval_date)),
            Some("2026-11".to_string())
        );
        assert_ne!(
            expense_period_code(expense_date),
            accounting_period_code(true, &period_of(approval_date)).unwrap_or_default()
        );
    }

    /// ⑪ 同年同月的报销：两个期间相同（回填 / 明示不影响常规单据的表现）。
    #[test]
    fn same_period_when_approved_in_expense_month() {
        let expense_date = date(2026, 11, 1);
        let approval_date = date(2026, 11, 20);
        assert_eq!(
            expense_period_code(expense_date),
            accounting_period_code(true, &period_of(approval_date)).unwrap_or_default()
        );
    }
}
