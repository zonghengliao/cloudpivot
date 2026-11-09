//! `budget_guard` 的纯函数单测：把 ARCH-6 §3.1.2 的口径钉死（不连库、不依赖 `ERP_DB_URL`）。
//!
//! 覆盖卡面要求的「预算校验」四类：额度内通过 / 超额度 / 无预算 / 额度刚好用完，
//! 外加两道闸各自的单边超支（年超月不超、年不超月超）与策略映射（OFF / WARN / BLOCK）。

use super::*;
use std::str::FromStr;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn verdict(
    mode: ControlMode,
    year_limit: &str,
    year_used: &str,
    month_limit: &str,
    month_used: &str,
    this: &str,
) -> BudgetVerdict {
    // 这批用例默认「槽里有生效预算」（额度或已用有任何一项即可视为编过）；
    // 「压根没编预算」的用例走 verdict_without_budget（判据是库里的 budget_id，不是额度）。
    let has_budget = [year_limit, year_used, month_limit, month_used]
        .iter()
        .any(|text| dec(text) > Decimal::ZERO);
    BudgetVerdict::evaluate(
        mode,
        has_budget.then(Uuid::nil),
        dec(year_limit),
        dec(year_used),
        dec(month_limit),
        dec(month_used),
        dec(this),
    )
}

/// 该槽**没有本期生效的 ACTIVE 预算**（`budget_id = None`）。
fn verdict_without_budget(mode: ControlMode, year_used: &str, this: &str) -> BudgetVerdict {
    BudgetVerdict::evaluate(
        mode,
        None,
        Decimal::ZERO,
        dec(year_used),
        Decimal::ZERO,
        Decimal::ZERO,
        dec(this),
    )
}

/// ① 额度内：年度、月度都够 → 不超、两个缺口都是 0。
#[test]
fn within_budget_passes() {
    let v = verdict(ControlMode::Warn, "100000", "20000", "10000", "3000", "5000");
    assert!(!v.over, "{}", v.describe());
    assert_eq!(v.year_gap, Decimal::ZERO);
    assert_eq!(v.month_gap, Decimal::ZERO);
    assert_eq!(v.year_available(), dec("80000"));
    assert_eq!(v.month_available(), dec("7000"));
    assert!(!v.blocks(), "WARN 模式永不阻断");
}

/// ② 年超、月不超：**只有年度缺口**（月度节奏没问题，说明年度总额被提前吃完）。
#[test]
fn year_over_month_within_reports_year_gap_only() {
    // 年额度 10 万、已用 9.8 万；月额度 5 万、已用 0；本次 5 千 → 年可用 2 千
    let v = verdict(ControlMode::Warn, "100000", "98000", "50000", "0", "5000");
    assert!(v.over);
    assert_eq!(v.year_gap, dec("3000"));
    assert_eq!(v.month_gap, Decimal::ZERO);
    assert_eq!(v.over_amount(), dec("3000"));
}

/// ③ 年不超、月超：**只有月度缺口**（总额还有钱，但这个月没额度了）。
#[test]
fn month_over_year_within_reports_month_gap_only() {
    // 年额度 10 万、已用 1 万；月额度 1 万、已用 9 千；本次 5 千 → 月可用 1 千
    let v = verdict(ControlMode::Warn, "100000", "10000", "10000", "9000", "5000");
    assert!(v.over);
    assert_eq!(v.year_gap, Decimal::ZERO);
    assert_eq!(v.month_gap, dec("4000"));
    assert_eq!(v.over_amount(), dec("4000"));
}

/// ④ 两道闸都超：`over_amount` 取**更严重**的那个（报文与快照都用它）。
#[test]
fn both_over_takes_the_worse_gap() {
    // 年可用 1 千、月可用 2 千，本次 5 千 → 年缺口 4 千、月缺口 3 千
    let v = verdict(ControlMode::Block, "10000", "9000", "10000", "8000", "5000");
    assert!(v.over);
    assert_eq!(v.year_gap, dec("4000"));
    assert_eq!(v.month_gap, dec("3000"));
    assert_eq!(v.over_amount(), dec("4000"), "取更大的缺口");
    assert!(v.blocks(), "BLOCK + 超支 = 阻断");
    assert!(v.describe().contains("超出预算"), "{}", v.describe());
}

/// ⑤ 无预算（该槽没有 ACTIVE 版本）：只标 `missing_budget`，**不算超支**（FIX-5）；
///    但 `OFF` 策略下当然也不阻断（忘编预算不等于停业，ARCH-6 D3）。
#[test]
fn missing_budget_is_flagged_but_never_over() {
    let warn = verdict(ControlMode::Warn, "0", "0", "0", "0", "1000");
    assert!(!warn.over, "没编预算 ≠ 超预算：{}", warn.describe());
    assert!(warn.missing_budget);
    assert_eq!(warn.over_amount(), Decimal::ZERO, "超支额 0，不是全额");
    assert_eq!(warn.over_amount_opt(), None, "落库口径是 NULL 而不是全额");
    assert!(!warn.blocks());

    let off = verdict(ControlMode::Off, "0", "0", "0", "0", "1000");
    assert!(!off.blocks(), "OFF 模式即使超支也不阻断（调用方也不会写 usage）");

    // FIX-5：BLOCK 下「未编预算」仍然拦（强管控 = 没有额度不许花），但理由不是「超支」
    let block = verdict(ControlMode::Block, "0", "0", "0", "0", "1000");
    assert!(block.missing_budget);
    assert!(!block.over, "未编预算不算超支");
    assert!(
        block.blocks(),
        "BLOCK 策略下未编预算仍要拦（否则等于把强管控放松成 WARN）"
    );
}

/// ⑥ 额度刚好用完：`本次 == 可用` → **不超**（边界，不能靠 `>=` 的直觉）。
#[test]
fn boundary_equal_is_not_over() {
    let v = verdict(ControlMode::Block, "10000", "6000", "10000", "6000", "4000");
    assert!(!v.over, "年/月都恰好用满，不该判超支：{}", v.describe());
    assert_eq!(v.year_available(), dec("4000"));
    assert!(!v.blocks());
    assert!(v.describe().contains("预算内"), "{}", v.describe());
}

/// ⑦ 一分钱就超（金额比较不能有四舍五入余量）。
#[test]
fn one_cent_over_is_over() {
    let v = verdict(ControlMode::Warn, "10000", "6000", "10000", "6000", "4000.01");
    assert!(v.over);
    assert_eq!(v.over_amount(), dec("0.01"));
}

/// ⑧ 已用 > 额度（调低预算后的历史超支）：可用截零，缺口按「本次全额」算。
#[test]
fn used_beyond_limit_clamps_available_to_zero() {
    let v = verdict(ControlMode::Warn, "10000", "12000", "10000", "12000", "500");
    assert_eq!(v.year_available(), Decimal::ZERO);
    assert_eq!(v.month_available(), Decimal::ZERO);
    assert_eq!(v.over_amount(), dec("500"));
    assert!(v.over);
}

/// ⑨ 策略解析：槽级优先、空则继承全局、非法值不静默猜测。
#[test]
fn control_mode_resolution() {
    assert_eq!(ControlMode::resolve(None, ControlMode::Warn), ControlMode::Warn);
    assert_eq!(
        ControlMode::resolve(Some("BLOCK"), ControlMode::Warn),
        ControlMode::Block,
        "槽级覆盖全局"
    );
    assert_eq!(
        ControlMode::resolve(Some("block"), ControlMode::Warn),
        ControlMode::Block,
        "大小写不敏感"
    );
    assert_eq!(
        ControlMode::resolve(Some("乱写"), ControlMode::Warn),
        ControlMode::Warn,
        "非法值回落到全局，不许静默当成 OFF"
    );
    assert!(ControlMode::parse("OFF").is_some());
    assert!(ControlMode::parse("NO").is_none());
    assert!(!ControlMode::Off.enforces());
    assert!(ControlMode::Warn.enforces() && ControlMode::Block.enforces());
}

/// ⑩ 槽构造：期间码非法一律 `None`（年份/月份越界、格式错）。
#[test]
fn slot_rejects_malformed_period() {
    let dept = Uuid::now_v7();
    let account = Uuid::now_v7();
    let slot = Slot::new(dept, account, "2026-03").expect("合法期间");
    assert_eq!(slot.fiscal_year, 2026);
    assert_eq!(slot.period_code, "2026-03");
    for bad in ["2026-13", "2026-00", "1999-01", "202603", "2026-3", ""] {
        assert!(Slot::new(dept, account, bad).is_none(), "非法期间 {bad} 应被拒");
    }
}

/// ⑪ 报文六个数齐全（排障时不用再翻库）。
#[test]
fn describe_carries_all_six_numbers() {
    let v = verdict(ControlMode::Warn, "100000", "20000", "10000", "3000", "5000");
    let text = v.describe();
    for needle in ["100000", "20000", "80000", "10000", "3000", "7000", "WARN"] {
        assert!(text.contains(needle), "报文缺 {needle}：{text}");
    }
}

/// ⑫ 策略词表：三个取值一个不多一个不少，`as_str` / `label` / `parse` 双向自洽。
///
/// 库里存的是 `as_str`（`biz_budget.control_mode` / `sys_config`），报文里用 `label`；
/// 任何一处漏改都会让「配了 BLOCK 却按 WARN 走」，所以逐值钉死。
#[test]
fn control_mode_vocabulary_round_trips() {
    assert_eq!(ControlMode::ALL.len(), 3, "取值域只能是 OFF/WARN/BLOCK");
    assert_eq!(ControlMode::ALL.map(ControlMode::as_str), ["OFF", "WARN", "BLOCK"]);
    assert_eq!(
        ControlMode::ALL.map(ControlMode::label),
        ["不校验", "标记并强制审批", "阻断提交"]
    );
    for mode in ControlMode::ALL {
        assert_eq!(
            ControlMode::parse(mode.as_str()),
            Some(mode),
            "as_str 必须能被 parse 还原：{}",
            mode.as_str()
        );
        assert!(!mode.label().is_empty());
    }
    // 落库值大小写不敏感 + 前后空白容忍（Excel / 配置页粘贴常带空格）
    assert_eq!(ControlMode::parse(" block "), Some(ControlMode::Block));
    assert_eq!(ControlMode::parse("\twarn\n"), Some(ControlMode::Warn));
    // 空串与未知值一律 None（调用方走默认，不许静默当 OFF）
    assert_eq!(ControlMode::parse(""), None);
    assert_eq!(ControlMode::parse("   "), None);
    assert_eq!(ControlMode::parse("ON"), None, "ON 不是合法取值");
}

/// ⑬ 槽级策略解析的空白边界：空串 / 纯空格也要继承全局，不能因「有个值」就当非法值丢掉全局。
#[test]
fn blank_slot_mode_inherits_global() {
    for raw in [None, Some(""), Some("   "), Some("\t"), Some("nonsense")] {
        assert_eq!(
            ControlMode::resolve(raw, ControlMode::Block),
            ControlMode::Block,
            "raw={raw:?} 应继承全局 BLOCK"
        );
    }
    assert_eq!(
        ControlMode::resolve(Some("off"), ControlMode::Block),
        ControlMode::Off,
        "槽级 OFF 覆盖全局 BLOCK（大小写不敏感）"
    );
}

/// ⑭ 年度边界：2000 与 2100 都算合法（闭区间），2101 与 1999 越过纪元窗。
#[test]
fn slot_year_window_is_inclusive() {
    let dept = Uuid::now_v7();
    let account = Uuid::now_v7();
    for (period, year) in [("2000-01", 2000), ("2100-12", 2100), ("2026-12", 2026)] {
        let slot = Slot::new(dept, account, period).unwrap_or_else(|| panic!("{period} 应合法"));
        assert_eq!(slot.fiscal_year, year, "{period} 年度推导错");
        assert_eq!(slot.year_text(), format!("{year} 年"));
    }
    for bad in ["1999-12", "2101-01", "2026-13", "2026-00", "abcd-03", "-2026-03"] {
        assert!(
            Slot::new(dept, account, bad).is_none(),
            "越界 / 非法的期间码 {bad} 应被拒"
        );
    }
    // 期间码按**位置**取年（0..4）与月（5..7），分隔符不参与校验：
    // `2026/03` 也能解析出正确的年月（历史数据里出现过手动改分隔符的期间码）。
    let slash = Slot::new(dept, account, "2026/03").expect("位置解析应容忍分隔符差异");
    assert_eq!(slash.fiscal_year, 2026);
    assert_eq!(slash.period_code, "2026/03", "原始期间码原样保留");
}

/// ⑮ `missing_budget` 的判据是**库里有没有本期生效的 ACTIVE 预算**（`budget_id`），
/// 不是「额度是否为 0」：额度 0 但有预算行 = 真超支；没预算行（哪怕历史有占用）= 未编制。
#[test]
fn missing_budget_follows_budget_row_not_limit() {
    let nothing = verdict_without_budget(ControlMode::Warn, "0", "100");
    assert!(nothing.missing_budget, "没有生效预算行 = 未编制");
    assert!(!nothing.over, "未编制预算不算超支");

    // 没预算行但历史上有占用（预算被删 / 上一版过期）：仍然是「未编制」，不是「超支」
    let spent_without_budget = verdict_without_budget(ControlMode::Warn, "500", "100");
    assert!(spent_without_budget.missing_budget);
    assert!(
        !spent_without_budget.over,
        "没编预算的槽被报过一次之后已用 > 0，不能因此变成「超支」（旧口径的坑）"
    );

    // 有预算行但额度被编成 0：这是「编了预算没给钱」→ 真超支
    let zero_limit = verdict(ControlMode::Warn, "0", "500", "0", "0", "100");
    assert!(!zero_limit.missing_budget, "有预算行就不是「未编制」");
    assert!(zero_limit.over, "额度 0 + 本次 100 → 全线超支");
    assert_eq!(zero_limit.over_amount(), dec("100"));

    let has_limit = verdict(ControlMode::Warn, "5000", "0", "0", "0", "100");
    assert!(!has_limit.missing_budget);
    // 月度额度没分解行（= 0）时月度闸按 0 判：年度还有钱但月度缺口是全额（ARCH-6 §3.1）
    assert_eq!(has_limit.month_gap, dec("100"), "月度额度 0 → 月度缺口全额");
    assert_eq!(has_limit.year_gap, Decimal::ZERO, "年度额度够，不计年缺口");
    assert!(has_limit.over, "月度闸先命中：{}", has_limit.describe());
}

/// ⑯ 0 元单永远不超支（任何额度下），即使该槽根本没预算。
#[test]
fn zero_amount_is_never_over() {
    for (limit, used) in [("0", "0"), ("100", "100"), ("100", "200")] {
        let v = verdict(ControlMode::Block, limit, used, limit, used, "0");
        assert!(!v.over, "本次 0 元不该超：{}", v.describe());
        assert_eq!(v.over_amount(), Decimal::ZERO);
        assert!(!v.blocks(), "0 元单在 BLOCK 模式下也不该被拦");
    }
}

/// ⑰ 三种结论的文案头：未编制 / 超支 / 预算内，且都带策略中文名。
#[test]
fn describe_branches_are_distinguishable() {
    let missing = verdict(ControlMode::Warn, "0", "0", "0", "0", "100");
    assert!(missing.describe().starts_with("未编制预算："));

    let over = verdict(ControlMode::Block, "1000", "900", "1000", "0", "500");
    assert!(over.describe().starts_with("超出预算（超支 "));

    let within = verdict(ControlMode::Off, "1000", "0", "1000", "0", "100");
    // 剩余 = min(年可用, 月可用)（不扣本次金额：本次金额单列在文案里）
    assert!(within.describe().contains("预算内（剩余 1000）"));
    for text in [missing.describe(), over.describe(), within.describe()] {
        assert!(
            text.contains("（") && text.contains("）"),
            "结论必须带策略中文名：{text}"
        );
    }
}

/// ⑱ `OFF` 仍如实算出缺口（只用于日志/预览），但永不阻断、也不该写占用。
#[test]
fn off_mode_still_reports_gaps_without_blocking() {
    let v = verdict(ControlMode::Off, "1000", "0", "1000", "0", "1500");
    assert!(v.over, "OFF 不校验但缺口照算，日志里要看得见");
    assert_eq!(v.over_amount(), dec("500"));
    assert!(!v.blocks(), "OFF 永不阻断");
    assert!(!v.mode.enforces(), "OFF 不占用");
}

/// ⑲ `over_amount` = 两个缺口里更大的那个，且恒 >= 0（额度调低后的负可用已被截零）。
#[test]
fn over_amount_is_max_and_never_negative() {
    // 只有年缺口：年可用 1000-900=100 → 缺口 500-100=400；月可用 10000-0 → 月缺口 0
    let only_year = verdict(ControlMode::Warn, "1000", "900", "10000", "0", "500");
    assert_eq!((only_year.year_gap, only_year.month_gap), (dec("400"), Decimal::ZERO));
    assert_eq!(only_year.over_amount(), dec("400"));
    // 只有月缺口：年可用 10000；月可用 1000-900=100 → 月缺口 400
    let only_month = verdict(ControlMode::Warn, "10000", "0", "1000", "900", "500");
    assert_eq!((only_month.year_gap, only_month.month_gap), (Decimal::ZERO, dec("400")));
    assert_eq!(only_month.over_amount(), dec("400"));
    // 两边都缺但大小不同：年缺口 100、月缺口 400 → 取大的（月）
    let both = verdict(ControlMode::Warn, "400", "0", "100", "0", "500");
    assert_eq!((both.year_gap, both.month_gap), (dec("100"), dec("400")));
    assert_eq!(both.over_amount(), dec("400"), "取 max 而不是 min/年缺口");
    let same = verdict(ControlMode::Warn, "1000", "900", "1000", "900", "500");
    assert_eq!(same.year_gap, same.month_gap, "两道闸一样紧");
    assert_eq!(same.over_amount(), dec("400"));
    // 已用远超额度（可用为负）时截零，缺口 = 本次金额本身，不是 本次 + 超编额度
    let negative_available = verdict(ControlMode::Warn, "100", "500", "100", "500", "50");
    assert_eq!(negative_available.year_gap, dec("50"));
    assert_eq!(negative_available.year_available(), Decimal::ZERO);
    assert_eq!(negative_available.over_amount(), dec("50"));
    let none = verdict(ControlMode::Warn, "10000", "0", "10000", "0", "1");
    assert_eq!(none.over_amount(), Decimal::ZERO);
    assert!(!none.over);
}

// ==================== FIX-2：effective_from 参与判定的口径 ====================

fn date(text: &str) -> NaiveDate {
    text.parse::<NaiveDate>().expect("日期字面量非法")
}

/// ⑳ 期间月末：平年 2 月 28、闰年 2 月 29、11 月 30、12 月 31（跨年进位不能错）。
#[test]
fn period_end_of_month_is_last_day() {
    assert_eq!(period_end_of("2026-11"), Some(date("2026-11-30")));
    assert_eq!(period_end_of("2026-02"), Some(date("2026-02-28")));
    assert_eq!(period_end_of("2024-02"), Some(date("2024-02-29")));
    assert_eq!(period_end_of("2026-12"), Some(date("2026-12-31")));
    assert_eq!(period_end_of("2026-01"), Some(date("2026-01-31")));
    // 非法期间码一律 None（调用方按「不生效」处理，绝不 panic）
    assert_eq!(period_end_of("2026-13"), None);
    assert_eq!(period_end_of("2026"), None);
    assert_eq!(period_end_of(""), None);
}

/// ㉑ **口径变更的钉子**（FIX-2 选项 A）：
/// `effective_from = 2026-12-01`（审批通过次月 1 日）→ 11 月的单据**不生效**，12 月才生效。
#[test]
fn effective_from_next_month_does_not_govern_this_month() {
    let slot_nov = Slot::new(Uuid::now_v7(), Uuid::now_v7(), "2026-11").expect("槽");
    let slot_dec = Slot::new(Uuid::now_v7(), Uuid::now_v7(), "2026-12").expect("槽");
    let effective_from = date("2026-12-01");
    assert_eq!(slot_nov.period_end(), Some(date("2026-11-30")), "11 月的月末是 11-30");
    assert!(
        effective_from > slot_nov.period_end().expect("月末"),
        "12-01 晚于 11-30 → 该版本对 11 月不生效（修复前 11 月就被 12 月额度管住）"
    );
    assert!(
        effective_from <= slot_dec.period_end().expect("月末"),
        "12-01 不晚于 12-31 → 12 月生效"
    );
    // 月末当天生效算生效（`<=` 不是 `<`）
    let same_day = date("2026-11-30");
    assert!(same_day <= slot_nov.period_end().expect("月末"));
}

/// ㉒ 非法期间码不会让 `period_end_text` 崩：退化成 None（SQL 侧用 9999-12-31 兜底）。
#[test]
fn invalid_period_has_no_end_text() {
    let slot = Slot {
        dept_id: Uuid::now_v7(),
        account_id: Uuid::now_v7(),
        fiscal_year: 2026,
        period_code: "bad".to_string(),
    };
    assert_eq!(slot.period_end(), None);
    assert_eq!(slot.period_end_text(), None);
}

// ==================== E2E06-FIX-2：月度闸公共公式 + 报表时点 ====================

/// ㉓ 月度闸公式（全系统唯一）：`over` / `over_amount` / `available` / 截零可用。
/// 报表的 `month_over / month_over_amount / month_available` 与提交侧的月度缺口都调它。
#[test]
fn month_gate_is_the_single_monthly_formula() {
    let under = MonthGate::of(dec("1000"), dec("999"));
    assert_eq!(under.available(), dec("1"));
    assert_eq!(under.available_clamped(), dec("1"));
    assert!(!under.over());
    assert_eq!(under.over_amount(), None);

    // 边界：用满不算超（与年度闸「M = 可用 不算超」一致）
    let equal = MonthGate::of(dec("1000"), dec("1000"));
    assert!(!equal.over(), "已用 == 额度 不算超");
    assert_eq!(equal.over_amount(), None);
    assert_eq!(equal.available(), Decimal::ZERO);

    // WARN 槽场景：月额度 1000，报了 1500
    let over = MonthGate::of(dec("1000"), dec("1500"));
    assert!(over.over());
    assert_eq!(over.over_amount(), Some(dec("500")), "超支额 = 已用 − 额度");
    assert_eq!(over.available(), dec("-500"), "带符号可用（报表展示这个）");
    assert_eq!(
        over.available_clamped(),
        Decimal::ZERO,
        "提交侧算缺口用截零值：不能把历史超支再算一次进本次缺口"
    );
}

/// ㉔ 提交侧（`BudgetVerdict::evaluate`）的月度缺口与 `MonthGate` 同源：
/// 逐一行遍 (额度, 已用, 本次) 组合，`month_gap == max(0, 本次 − 截零可用)`；
/// 且 `MonthGate::over` 为真时（本次 > 0）提交侧必定判超支。
#[test]
fn verdict_month_gap_comes_from_month_gate() {
    for (limit, used) in [
        ("1000", "0"),
        ("1000", "600"),
        ("1000", "1000"),
        ("1000", "1500"),
        ("0", "0"),
        ("0", "300"),
    ] {
        let gate = MonthGate::of(dec(limit), dec(used));
        for this in ["1", "100", "1000", "5000"] {
            let v = BudgetVerdict::evaluate(
                ControlMode::Warn,
                Some(Uuid::now_v7()),
                dec("100000"),
                Decimal::ZERO,
                gate.limit,
                gate.used,
                dec(this),
            );
            let expected = (dec(this) - gate.available_clamped()).max(Decimal::ZERO);
            assert_eq!(
                v.month_gap, expected,
                "月度缺口必须 = max(0, 本次 − MonthGate 截零可用)（limit={limit} used={used} this={this}）"
            );
            assert_eq!(v.month_available(), gate.available_clamped(), "month_available 同源");
            if gate.over() {
                assert!(v.over, "月度闸已超 → 提交侧必须判超支（limit={limit} used={used}）");
            }
        }
    }
}

/// ㉕ 会计年度期末 = 该年 12-31（预算按自然年编制）；越界年份（库内 CHECK 2000..=2100）
/// 一律 `None`，与 `Slot::new` 的窗口一致。
#[test]
fn fiscal_year_end_is_december_31() {
    assert_eq!(fiscal_year_end(2027), Some(date("2027-12-31")));
    assert_eq!(fiscal_year_end(2024), Some(date("2024-12-31")));
    // 窗口边界（含端点）
    assert_eq!(fiscal_year_end(FISCAL_YEAR_MIN), Some(date("2000-12-31")));
    assert_eq!(fiscal_year_end(FISCAL_YEAR_MAX), Some(date("2100-12-31")));
    // 越界不 panic，也不给出一个「像真的一样」的日期
    assert_eq!(fiscal_year_end(0), None);
    assert_eq!(fiscal_year_end(-1), None);
    assert_eq!(fiscal_year_end(FISCAL_YEAR_MIN - 1), None);
    assert_eq!(fiscal_year_end(FISCAL_YEAR_MAX + 1), None);
}

/// ㉖ **报表 `effective` 的口径钉子**（卡面验收 ③④）：
/// 判定时点是「该预算所属年度的期末」，不是「今天」。
#[test]
fn report_effective_is_judged_at_fiscal_year_end() {
    // ③ FY2027 预算、审批通过次月生效 2026-12-01 → 报表 fiscal_year=2027 时 effective=true
    //    （修复前用「本月月末 2026-11-30」→ 显示未生效，与 2027-02 已被控制自相矛盾）
    assert!(
        effective_in_year(Some(date("2026-12-01")), 2027),
        "2026-12-01 ≤ 2027-12-31 → FY2027 这份预算在这一年算数"
    );
    // ④ FY2027 预算、effective_from 2028-06-01 → 报表 fiscal_year=2027 时 effective=false
    assert!(
        !effective_in_year(Some(date("2028-06-01")), 2027),
        "2028-06-01 > 2027-12-31 → 这一年根本用不上"
    );
    // 期末当天生效算生效（`<=` 不是 `<`）
    assert!(effective_in_year(Some(date("2027-12-31")), 2027));
    // 历史数据没有生效日 → 视为已生效（与 slot_snapshot 的 NULL 放行同口径）
    assert!(effective_in_year(None, 2027));
    // 年初生效当然算
    assert!(effective_in_year(Some(date("2027-01-01")), 2027));
    // 上一年年末生效也算（12 月批、次年 1 月生效）
    assert!(effective_in_year(Some(date("2026-12-01")), 2027));
    // 与期间月末口径的差别只在粒度：同一份预算对 2026-11 不生效、对 2027 整年生效
    let nov_end = period_end_of("2026-11").expect("月末");
    assert!(date("2026-12-01") > nov_end, "控制侧：11 月的单据不被它管");
    assert!(
        effective_in_year(Some(date("2026-12-01")), 2027),
        "报表侧：FY2027 这一年算数"
    );
}

// ==================== FIX-2：记账口径（budget_mode 取槽级最严） ====================

fn slot_check(dept: Uuid, account: Uuid, mode: ControlMode, amount: &str) -> ExpenseSlotCheck {
    let slot = Slot::new(dept, account, "2026-11").expect("槽");
    ExpenseSlotCheck {
        slot,
        amount: dec(amount),
        amount_net: dec(amount),
        verdict: verdict(mode, "1000", "0", "1000", "0", amount),
        snapshot: SlotSnapshot {
            budget_id: None,
            year_limit: dec("1000"),
            year_used: Decimal::ZERO,
            month_limit: dec("1000"),
            month_used: Decimal::ZERO,
        },
        account_code: Some("5602.02".to_string()),
        account_name: Some("差旅费".to_string()),
        mode_source: ModeSource::Slot,
    }
}

/// ㉓ 全局 WARN + 槽级 BLOCK → 单据记账口径必须是 **BLOCK**（问题 4 的钉子）。
#[test]
fn recorded_mode_takes_strictest_slot_mode() {
    let dept = Uuid::now_v7();
    let budget = ExpenseBudget {
        mode: ControlMode::Warn, // 全局默认
        dept_id: dept,
        period_code: "2026-11".to_string(),
        checks: vec![
            slot_check(dept, Uuid::now_v7(), ControlMode::Block, "1500"),
            slot_check(dept, Uuid::now_v7(), ControlMode::Warn, "100"),
        ],
        reason: String::new(),
    };
    assert_eq!(budget.recorded_mode(), ControlMode::Block);
    assert_eq!(budget.recorded_mode().as_str(), "BLOCK");
    assert!(budget.blocks(), "槽级 BLOCK 必须拦住");

    // 没有受控槽 → 回落到全局（与修复前行为一致）
    let empty = ExpenseBudget {
        mode: ControlMode::Warn,
        dept_id: dept,
        period_code: "2026-11".to_string(),
        checks: Vec::new(),
        reason: String::new(),
    };
    assert_eq!(empty.recorded_mode(), ControlMode::Warn);

    // 槽级 OFF（不校验）不参与记账，取有校验的 WARN
    let mixed = ExpenseBudget {
        mode: ControlMode::Warn,
        dept_id: dept,
        period_code: "2026-11".to_string(),
        checks: vec![
            slot_check(dept, Uuid::now_v7(), ControlMode::Off, "100"),
            slot_check(dept, Uuid::now_v7(), ControlMode::Warn, "100"),
        ],
        reason: String::new(),
    };
    assert_eq!(mixed.recorded_mode(), ControlMode::Warn);
}

/// ㉔ 严格度排序：OFF < WARN < BLOCK（`max_by_key` 取的就是它）。
#[test]
fn strictness_orders_off_warn_block() {
    assert!(ControlMode::Off.strictness() < ControlMode::Warn.strictness());
    assert!(ControlMode::Warn.strictness() < ControlMode::Block.strictness());
}

// ==================== FIX-2：未生效预算的提示文案 ====================

fn pending(status: &str, effective_from: Option<NaiveDate>) -> PendingBudget {
    PendingBudget {
        account_id: Uuid::now_v7(),
        account_code: "5602.02".to_string(),
        budget_no: "BDG-20261107-000001".to_string(),
        status: status.to_string(),
        effective_from,
        total_amount: dec("10000"),
    }
}

/// ㉕ DRAFT / SUBMITTED / ACTIVE 未到生效日 → 三种提示都在，码统一、文案可读。
#[test]
fn pending_warnings_cover_three_states() {
    let rows = vec![
        pending("DRAFT", None),
        pending("SUBMITTED", None),
        pending("ACTIVE", Some(date("2026-12-01"))),
    ];
    let warnings = pending_warnings(&rows, "2026-11");
    assert_eq!(warnings.len(), 3);
    for warning in &warnings {
        assert_eq!(warning.code, WARN_BUDGET_NOT_EFFECTIVE);
        assert!(
            warning.message.contains("预算尚未生效"),
            "文案要直说「未生效」：{}",
            warning.message
        );
        assert!(
            warning.message.contains("2026-11"),
            "文案要带上受影响的期间：{}",
            warning.message
        );
        assert!(
            warning.message.contains("BDG-20261107-000001"),
            "文案要能定位到预算单：{}",
            warning.message
        );
    }
    assert_eq!(warnings[0].status_label, "草稿");
    assert!(warnings[0].message.contains("请提交审批"));
    assert_eq!(warnings[1].status_label, "审批中");
    assert_eq!(warnings[2].status_label, "已生效（未到生效日）");
    assert!(
        warnings[2].message.contains("2026-12-01 起生效"),
        "未到生效日的提示要说清哪天生效：{}",
        warnings[2].message
    );
    // 空输入 → 空输出（没编预算不该冒出提示）
    assert!(pending_warnings(&[], "2026-11").is_empty());
}

/// ㉖ 提示必须带上额度与期间 —— 用户看到的是「本来能管多少、哪个月没管住」。
#[test]
fn pending_warning_message_carries_amount_and_period() {
    let warnings = pending_warnings(&[pending("DRAFT", None)], "2026-11");
    assert!(warnings[0].message.contains("10000"));
    assert!(warnings[0].message.contains("2026-11"));
}

// ==================== FIX-5：409 报文按槽说话 + 未编预算不冒充超支 ====================

/// 造一个「已用 ~ 额度」的槽（`used` 与 `limit` 由用例给，便于钉报文数字）。
fn slot_of(
    account_code: &str,
    account_name: &str,
    mode: ControlMode,
    mode_source: ModeSource,
    used: &str,
    limit: &str,
    this: &str,
) -> ExpenseSlotCheck {
    let slot = Slot::new(Uuid::now_v7(), Uuid::now_v7(), "2026-11").expect("槽");
    ExpenseSlotCheck {
        slot,
        amount: dec(this),
        amount_net: dec(this),
        verdict: verdict(mode, limit, used, limit, used, this),
        snapshot: SlotSnapshot {
            budget_id: None,
            year_limit: dec(limit),
            year_used: dec(used),
            month_limit: dec(limit),
            month_used: dec(used),
        },
        account_code: Some(account_code.to_string()),
        account_name: Some(account_name.to_string()),
        mode_source,
    }
}

fn expense_budget(global: ControlMode, checks: Vec<ExpenseSlotCheck>) -> ExpenseBudget {
    ExpenseBudget {
        mode: global,
        dept_id: Uuid::now_v7(),
        period_code: "2026-11".to_string(),
        checks,
        reason: String::new(),
    }
}

/// ㉗ 问题 1 的钉子：全局 WARN + 槽级 BLOCK 超支 → 报文必须说 **槽级策略 BLOCK**，
/// 绝不能再说「标记并强制审批」（那是全局 WARN 的标签，用户看不懂为什么被拦）。
#[test]
fn block_reason_says_slot_level_block_not_global_label() {
    let budget = expense_budget(
        ControlMode::Warn,
        vec![
            slot_of(
                "5602.02",
                "差旅费",
                ControlMode::Block,
                ModeSource::Slot,
                "9500",
                "10000",
                "1000",
            ),
            slot_of(
                "5602.03",
                "办公费",
                ControlMode::Warn,
                ModeSource::Slot,
                "0",
                "8000",
                "500",
            ),
        ],
    );
    assert!(budget.blocks(), "槽级 BLOCK + 超支必须阻断");
    let reason = budget.block_reason().expect("有阻断槽就有正文");
    assert!(reason.contains("槽级策略 BLOCK"), "报文：{reason}");
    assert!(reason.contains("科目「5602.02 差旅费」2026年11月"), "报文：{reason}");
    assert!(reason.contains("月度已用 9500 / 月度预算 10000"), "报文：{reason}");
    assert!(reason.contains("本次 1000"), "报文：{reason}");
    assert!(reason.contains("超支 500"), "报文：{reason}");
    assert!(
        !reason.contains("标记并强制审批"),
        "全局 WARN 的标签不能用来解释槽级阻断：{reason}"
    );
    // 只列阻断的那个槽（另一个在额度内，不该混进 409 正文）
    assert_eq!(reason.lines().count(), 1, "只列阻断槽：{reason}");
    assert_eq!(budget.block_headline(), "超出预算额度", "超支的第一句");
}

/// ㉘ 槽级没配策略（继承全局）时，报文要说「全局策略 BLOCK」，不能张冠李戴成槽级。
#[test]
fn block_reason_says_global_policy_when_inherited() {
    let budget = expense_budget(
        ControlMode::Block,
        vec![slot_of(
            "5602.01",
            "基本工资",
            ControlMode::Block,
            ModeSource::Global,
            "10000",
            "10000",
            "3500",
        )],
    );
    let reason = budget.block_reason().expect("正文");
    assert!(reason.contains("全局策略 BLOCK"), "报文：{reason}");
    assert!(!reason.contains("槽级策略"), "报文：{reason}");
    assert!(reason.contains("超支 3500"), "报文：{reason}");
}

/// ㉙ 多个槽都阻断：逐槽列出，超支额大的排前面（业务先看最严重的）。
#[test]
fn block_reason_lists_every_blocking_slot_worst_first() {
    let budget = expense_budget(
        ControlMode::Block,
        vec![
            slot_of(
                "5602.05",
                "招待费",
                ControlMode::Block,
                ModeSource::Slot,
                "1000",
                "1000",
                "200",
            ),
            slot_of(
                "5602.02",
                "差旅费",
                ControlMode::Block,
                ModeSource::Slot,
                "10000",
                "10000",
                "3500",
            ),
        ],
    );
    let reason = budget.block_reason().expect("正文");
    let lines: Vec<&str> = reason.lines().collect();
    assert_eq!(lines.len(), 2, "两个阻断槽各一行：{reason}");
    assert!(lines[0].contains("差旅费"), "超支 3500 的排在前面：{reason}");
    assert!(lines[1].contains("招待费"), "报文：{reason}");
    assert!(
        lines.iter().all(|line| line.starts_with("· ")),
        "逐槽用 · 起行：{reason}"
    );
}

/// ㉚ 未编预算的槽：正文说「未编制预算」，既不说超支也不说策略 —— 它不是阻断原因。
#[test]
fn missing_budget_slot_describe_is_not_a_block() {
    let missing = slot_of(
        "5602.09",
        "培训费",
        ControlMode::Warn,
        ModeSource::Global,
        "0",
        "0",
        "800",
    );
    assert!(missing.verdict.missing_budget);
    assert!(!missing.verdict.over, "没编预算不算超支");
    let text = missing.describe();
    assert!(text.contains("未编制预算"), "文案：{text}");
    assert!(!text.contains("，超支"), "文案：{text}");
    assert!(!text.contains("BLOCK"), "文案：{text}");

    // WARN：整单只有未编预算的槽 → 放行（只标记），没有阻断正文
    let warn = expense_budget(ControlMode::Warn, vec![missing]);
    assert!(!warn.blocks(), "WARN 下未编预算放行（只标记）");
    assert!(warn.block_reason().is_none());
    assert!(warn.missing_budget());
    assert!(!warn.over());
    assert_eq!(warn.over_amount_opt(), None, "落库写 NULL");

    // BLOCK：同一张单 → 拦，但第一句说「未编制预算」而不是「超出预算额度」
    let missing_block = slot_of(
        "5602.09",
        "培训费",
        ControlMode::Block,
        ModeSource::Slot,
        "0",
        "0",
        "800",
    );
    let block = expense_budget(ControlMode::Block, vec![missing_block]);
    assert!(block.blocks(), "BLOCK 下未编预算必拦（强管控不许放松）");
    assert_eq!(
        block.block_headline(),
        "涉及的预算科目未编制（当前策略 BLOCK，不予放行）"
    );
    let reason = block.block_reason().expect("阻断正文");
    assert!(reason.contains("未编制预算"), "报文：{reason}");
    assert!(reason.contains("槽级策略 BLOCK"), "报文：{reason}");
}

/// ㉛ 额度内的槽：正文说「额度内」并给剩余额度（报文能自证没超）。
#[test]
fn within_budget_slot_describe_shows_remainder() {
    let ok = slot_of(
        "5602.03",
        "办公费",
        ControlMode::Warn,
        ModeSource::Slot,
        "2000",
        "10000",
        "500",
    );
    let text = ok.describe();
    assert!(text.contains("额度内"), "文案：{text}");
    assert!(text.contains("剩余 8000"), "文案：{text}");
}

/// ㉜ 报文helper：金额去尾零、期间码转中文月份（非法输入原样回显）。
#[test]
fn amount_and_period_text_are_human_readable() {
    assert_eq!(amount_text(dec("3500.0000")), "3500");
    assert_eq!(amount_text(dec("833.3300")), "833.33");
    assert_eq!(amount_text(Decimal::ZERO), "0");
    assert_eq!(period_label("2026-11"), "2026年11月");
    assert_eq!(period_label("2026-01"), "2026年1月");
    assert_eq!(period_label("bad"), "bad", "非法期间原样回显，不 panic");
}
