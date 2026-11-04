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
    BudgetVerdict::evaluate(
        mode,
        dec(year_limit),
        dec(year_used),
        dec(month_limit),
        dec(month_used),
        dec(this),
    )
}

/// ① 额度内：年度、月度都够 → 不超、两个缺口都是 0。
#[test]
fn within_budget_passes() {
    let v = verdict(
        ControlMode::Warn,
        "100000",
        "20000",
        "10000",
        "3000",
        "5000",
    );
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
    let v = verdict(
        ControlMode::Warn,
        "100000",
        "10000",
        "10000",
        "9000",
        "5000",
    );
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

/// ⑤ 无预算（该槽没有 ACTIVE 版本）：额度按 0 → `missing_budget` 且超支；
///    但 `OFF` 策略下不阻断（忘编预算不等于停业，ARCH-6 D3）。
#[test]
fn missing_budget_is_flagged_but_off_mode_never_blocks() {
    let warn = verdict(ControlMode::Warn, "0", "0", "0", "0", "1000");
    assert!(warn.over);
    assert!(warn.missing_budget);
    assert_eq!(warn.over_amount(), dec("1000"));
    assert!(!warn.blocks());

    let off = verdict(ControlMode::Off, "0", "0", "0", "0", "1000");
    assert!(
        !off.blocks(),
        "OFF 模式即使超支也不阻断（调用方也不会写 usage）"
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
    let v = verdict(
        ControlMode::Warn,
        "10000",
        "6000",
        "10000",
        "6000",
        "4000.01",
    );
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
    assert_eq!(
        ControlMode::resolve(None, ControlMode::Warn),
        ControlMode::Warn
    );
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
        assert!(
            Slot::new(dept, account, bad).is_none(),
            "非法期间 {bad} 应被拒"
        );
    }
}

/// ⑪ 报文六个数齐全（排障时不用再翻库）。
#[test]
fn describe_carries_all_six_numbers() {
    let v = verdict(
        ControlMode::Warn,
        "100000",
        "20000",
        "10000",
        "3000",
        "5000",
    );
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
    assert_eq!(
        ControlMode::ALL.map(ControlMode::as_str),
        ["OFF", "WARN", "BLOCK"]
    );
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
    for bad in [
        "1999-12", "2101-01", "2026-13", "2026-00", "abcd-03", "-2026-03",
    ] {
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

/// ⑮ `missing_budget` 只看「年度额度与年度已用都 <= 0」：额度为 0 但有历史占用不算「未编制」。
#[test]
fn missing_budget_only_when_no_limit_and_no_usage() {
    let nothing = verdict(ControlMode::Warn, "0", "0", "0", "0", "100");
    assert!(nothing.missing_budget, "额度 0 + 已用 0 = 未编制预算");

    let spent_without_limit = verdict(ControlMode::Warn, "0", "500", "0", "0", "100");
    assert!(
        !spent_without_limit.missing_budget,
        "有历史占用说明预算曾经存在过，不能报「未编制」"
    );

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
    assert_eq!(
        (only_year.year_gap, only_year.month_gap),
        (dec("400"), Decimal::ZERO)
    );
    assert_eq!(only_year.over_amount(), dec("400"));
    // 只有月缺口：年可用 10000；月可用 1000-900=100 → 月缺口 400
    let only_month = verdict(ControlMode::Warn, "10000", "0", "1000", "900", "500");
    assert_eq!(
        (only_month.year_gap, only_month.month_gap),
        (Decimal::ZERO, dec("400"))
    );
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
