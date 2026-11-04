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
