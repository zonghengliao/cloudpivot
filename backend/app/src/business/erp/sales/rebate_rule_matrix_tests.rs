//! 销售返利 · **三种规则 × 边界** 的纯逻辑单测矩阵（M3-18）。
//!
//! # 本卡钉什么
//!
//! 一句话：「按量（BY_QTY）/ 按额（BY_AMOUNT）/ 阶梯（TIERED）」三种规则，
//! 在**形状校验**（[`checked_rules`]）与**落档计价**（[`calc_rebate`]）两段上，
//! 每一个「刚好等于边界」和「刚过多一分」的取值都给出确定答案。
//!
//! ```text
//! 段 1  形状      三种类型各自允许/禁止的档位排布（18 行表驱动，含错误码 66089）
//! 段 2  数值上限  精确到库列（NUMERIC(18,4)/(18,6)）的「恰好装得下 / 多一位就拒」
//! 段 3  落档      左闭右开，每个 tier_from / tier_to 的 ±0.0001 都测
//! 段 4  计价      三种规则在边界基数上的应返金额（2 位、不累进、恒 ≥ 0）
//! 段 5  分位      0.005 / 0.015 / 0.025 这类「正好落在半分上」的舍入策略
//!                 （**现状是银行家舍入**：0.025 → 0.02，段 5 里有说明与理由）
//! 段 6  空与负    无单、纯退货、净额恰好 0 —— 三种情形各自的 warning 条数与金额
//! 段 7  线上路径  RebateRuleReq → to_input → checked_rules → calc_rebate
//!                 （handler 的两步走照抄一遍，证明「接口收的」与「算出来的」一致）
//! 段 8  组合配对  3 类型 × 2 单位 = 6 组合，只有 4 组合能算
//! 段 9  不变量    基数递增 → 应返不递减；应返恒 ≥ 0、精度 ≤ 2 位、money2 幂等；
//!                 落档基数 = max(净额, 0)
//! 段 10 期间边界  月度/季度、跨年、闰年 2 月
//! ```
//!
//! # 为什么不连库
//!
//! 单测就该秒级、无依赖。真库链路（政策落库 → 结算单 → 红字发票）已由
//! `rebate_db_tests` / `rebate_settle_db_tests` / `rebate_invoice_db_tests` 三份
//! 带 `ERP_DB_URL` 开关的用例覆盖；本文件只钉**纯函数**，`cargo test --lib` 直接可跑。
//!
//! # 边界值的来源
//!
//! - 档位/比率的精度与上限来自 `rebate_dto` 的 `QTY_LIMIT` / `RATE_LIMIT` /
//!   `PERCENT_MAX`，与迁移 V0438 的列定义 `NUMERIC(18,4)` / `NUMERIC(18,6)` 同源；
//! - 落档口径来自 V0438 建表注释与 [`pick_tier`] 的文档：**左闭右开、逐档命中、不累进**。

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;

use super::rebate_dto::{
    self, MAX_TIERS, MIN_TIER_TIERS, RATE_UNITS, RULE_BY_AMOUNT, RULE_BY_QTY, RULE_TIERED, RULE_TYPES, RebateRuleReq,
    RuleInput, RuleWrite, UNIT_PER_UNIT, UNIT_PERCENT, checked_rules, checked_unit, pick_tier,
};
use super::rebate_settle_dto::{BaseNumbers, PeriodArgs, STATUS_DRAFT, STATUS_SETTLED, calc_rebate, money2};
use cloudpivot_kernel::shared::error_code;
use nik::prelude::chrono::NaiveDate;

// ==================== 夹具 ====================

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试字面量必须是合法小数")
}

fn tier(from: &str, to: Option<&str>, rate: &str) -> RuleInput {
    RuleInput {
        tier_from: dec(from),
        tier_to: to.map(dec),
        rate: dec(rate),
        remark: None,
    }
}

/// 已校验的档位（走 `checked_rules` 生成，保证与线上同源，不手搓 `RuleWrite`）。
fn rules_of(rule_type: &str, unit: &str, tiers: &[RuleInput]) -> Vec<RuleWrite> {
    checked_rules(rule_type, unit, tiers).expect("夹具必须是合法档位")
}

/// 只填销量的基数（按量/按量阶梯用）。
fn base_qty(qty: &str) -> BaseNumbers {
    BaseNumbers {
        order_count: 1,
        order_amount: dec("0"),
        order_qty: dec(qty),
        ..Default::default()
    }
}

/// 只填销售额的基数（按额/按额阶梯用）。
fn base_amount(amount: &str) -> BaseNumbers {
    BaseNumbers {
        order_count: 1,
        order_amount: dec(amount),
        order_qty: dec("1"),
        ..Default::default()
    }
}

/// 三档阶梯（按量或按额皆可）：[0,100) 0.5 / [100,500) 1 / [500,∞) 2。
fn three_tiers() -> Vec<RuleInput> {
    vec![
        tier("0", Some("100"), "0.5"),
        tier("100", Some("500"), "1"),
        tier("500", None, "2"),
    ]
}

// ==================== 段 1：三种规则的形状矩阵 ====================

/// 18 行形状矩阵：三种类型各自「允许什么」「禁止什么」，全部 400 + 66089 并点名原因。
///
/// 判据（与 V0438 建表注释同一份口径）：
/// `BY_QTY`/`BY_AMOUNT` 恰好 1 档且平铺；`TIERED` 2~20 档、从 0 起、首尾相接、末档开口。
#[test]
fn shape_matrix_three_rules() {
    struct Case {
        name: &'static str,
        rule_type: &'static str,
        unit: &'static str,
        tiers: Vec<RuleInput>,
        /// `Ok(期望档数)` 或 `Err(文案里必须出现的片段)`
        expected: std::result::Result<usize, &'static str>,
    }

    let cases = vec![
        Case {
            name: "按量·单档平铺",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", None, "0.5")],
            expected: Ok(1),
        },
        Case {
            name: "按额·单档平铺",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: vec![tier("0", None, "2.5")],
            expected: Ok(1),
        },
        Case {
            name: "阶梯·两档",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1"), tier("100", None, "2")],
            expected: Ok(2),
        },
        Case {
            name: "阶梯·三档乱序入参（服务端重排）",
            rule_type: RULE_TIERED,
            unit: UNIT_PERCENT,
            tiers: three_tiers(),
            expected: Ok(3),
        },
        Case {
            name: "阶梯·恰好 20 档（上限）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: (0..MAX_TIERS)
                .map(|index| {
                    let from = index * 10;
                    let to = if index + 1 == MAX_TIERS {
                        None
                    } else {
                        Some((index + 1) * 10)
                    };
                    RuleInput {
                        tier_from: Decimal::from(from),
                        tier_to: to.map(Decimal::from),
                        rate: dec("1"),
                        remark: None,
                    }
                })
                .collect(),
            expected: Ok(MAX_TIERS),
        },
        Case {
            name: "按量·两档（应改用阶梯）",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1"), tier("100", None, "2")],
            expected: Err("阶梯"),
        },
        Case {
            name: "按额·设起征区间",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: vec![tier("1000", None, "2")],
            expected: Err("TIERED"),
        },
        Case {
            name: "按量·有上限（非平铺）",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1")],
            expected: Err("平铺"),
        },
        Case {
            name: "阶梯·只有一档（伪装成阶梯）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", None, "1")],
            expected: Err("至少要 2 档"),
        },
        Case {
            name: "阶梯·首档不从 0 起",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("10", Some("100"), "1"), tier("100", None, "2")],
            expected: Err("从 0 起"),
        },
        Case {
            name: "阶梯·中间断档",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1"), tier("200", None, "2")],
            expected: Err("相接"),
        },
        Case {
            name: "阶梯·末档带上限（超上限无政策）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1"), tier("100", Some("200"), "2")],
            expected: Err("最后一档"),
        },
        Case {
            name: "阶梯·中间档没写上限",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![
                tier("0", None, "1"),
                tier("100", Some("200"), "2"),
                tier("200", None, "3"),
            ],
            expected: Err("没写上限"),
        },
        Case {
            name: "阶梯·下限重复",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("100"), "1"), tier("0", None, "2")],
            expected: Err("重复"),
        },
        Case {
            name: "阶梯·空区间上下限相等",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", Some("0"), "1"), tier("0", None, "2")],
            expected: Err("区间不成立"),
        },
        Case {
            name: "一档都没有",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: vec![],
            expected: Err("至少要有 1 档"),
        },
        Case {
            name: "超过 20 档上限",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: (0..=MAX_TIERS)
                .map(|index| RuleInput {
                    tier_from: Decimal::from(index),
                    tier_to: None,
                    rate: dec("1"),
                    remark: None,
                })
                .collect(),
            expected: Err("最多 20 档"),
        },
        Case {
            name: "比率为 0",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: vec![tier("0", None, "0")],
            expected: Err("必须大于 0"),
        },
    ];

    for case in &cases {
        let actual = checked_rules(case.rule_type, case.unit, &case.tiers);
        match (case.expected, &actual) {
            (Ok(count), Ok(rules)) => {
                assert_eq!(rules.len(), count, "【{}】档数不对", case.name);
                // 契约：输出恒按 tier_from 升序、seq = 1..N
                assert_eq!(
                    rules.iter().map(|rule| rule.seq).collect::<Vec<_>>(),
                    (1..=count as i16).collect::<Vec<_>>(),
                    "【{}】seq 必须从 1 连续编号",
                    case.name
                );
                assert!(
                    rules.windows(2).all(|pair| pair[0].tier_from < pair[1].tier_from),
                    "【{}】输出必须按 tier_from 升序",
                    case.name
                );
                assert!(
                    rules.last().is_some_and(|rule| rule.tier_to.is_none()),
                    "【{}】末档必须开口",
                    case.name
                );
            }
            (Err(fragment), Err(error)) => {
                assert_eq!(error.status, 400, "【{}】必须 400", case.name);
                assert_eq!(
                    error.code,
                    error_code::ERP_REBATE_RULE_INVALID,
                    "【{}】必须 66089",
                    case.name
                );
                assert!(
                    error.msg.contains(fragment),
                    "【{}】文案里应出现「{fragment}」，实际：{}",
                    case.name,
                    error.msg
                );
            }
            (expected, actual) => panic!(
                "【{}】期望 {:?}，实际 {:?}",
                case.name,
                expected.map(|count| format!("Ok({count}) 档")).map_err(str::to_string),
                actual
                    .as_ref()
                    .map(|rules| format!("Ok({} 档)", rules.len()))
                    .map_err(|error| error.msg.clone())
            ),
        }
    }
    println!(
        "[M3-18] 形状矩阵 {} 行：三种规则允许/禁止排布全部按 66089 收口 ✓",
        cases.len()
    );
}

// ==================== 段 2：数值上限（库列精度） ====================

/// 档位 `NUMERIC(18,4)`、比率 `NUMERIC(18,6)`：**恰好装得下**与**多一位就拒**两侧都测。
#[test]
fn numeric_column_limits_are_exact() {
    // —— 档位：整数部分上限 10^14（= 100000000000000），多一分都装不进 NUMERIC(18,4)
    // 用两档阶梯测：按量（平铺）的 tier_from 恒为 0、tier_to 恒为空，测不到这两个位置。
    let max_tier = "99999999999999.9999";
    let at_limit = vec![tier("0", Some(max_tier), "1"), tier(max_tier, None, "1")];
    assert!(
        checked_rules(RULE_TIERED, UNIT_PER_UNIT, &at_limit).is_ok(),
        "{max_tier} 恰好装得下 NUMERIC(18,4)（tier_from 与 tier_to 两个位置）"
    );
    let over_limit = vec![
        tier("0", Some("100000000000000"), "1"),
        tier("100000000000000", None, "1"),
    ];
    let error = checked_rules(RULE_TIERED, UNIT_PER_UNIT, &over_limit).expect_err("10^14 已越界");
    assert!(error.msg.contains("整数上限"), "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

    // —— 档位小数位：4 位可以、5 位拒（多的位会被库静默四舍五入，不能静默）
    let four_dp = vec![tier("0", Some("0.0001"), "1"), tier("0.0001", None, "1")];
    assert!(
        checked_rules(RULE_TIERED, UNIT_PER_UNIT, &four_dp).is_ok(),
        "4 位小数合法（tier_from/tier_to 都是 0.0001）"
    );
    let five_dp = vec![tier("0", Some("0.00001"), "1"), tier("0.00001", None, "1")];
    let error = checked_rules(RULE_TIERED, UNIT_PER_UNIT, &five_dp).expect_err("5 位小数");
    assert!(error.msg.contains("4 位小数"), "{}", error.msg);

    // —— 比率：元/件上限 10^12（NUMERIC(18,6)）
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "999999999999.999999")]).is_ok(),
        "比率恰好装得下 NUMERIC(18,6)"
    );
    let error = checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "1000000000000")]).expect_err("比率越界");
    assert!(error.msg.contains("整数上限"), "{}", error.msg);

    // —— 比率小数位：6 位可以、7 位拒
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "0.000001")]).is_ok(),
        "6 位小数合法"
    );
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "0.0000001")]).is_err(),
        "7 位小数必须拒"
    );

    // —— 百分比上限 100：100 可以、101 拒
    assert!(
        checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[tier("0", None, "100")]).is_ok(),
        "100% 是合法上限（全返）"
    );
    let error = checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[tier("0", None, "101")]).expect_err("101%");
    assert!(error.msg.contains("不能超过 100"), "{}", error.msg);

    // —— 元/件不受 100 限制（大额返利是合法业务）
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "500")]).is_ok(),
        "元/件 500 元是合法政策"
    );

    // —— 负数：档位与比率都拒（负返利走红冲，不走政策）
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("-1", None, "1")]).is_err(),
        "负档位"
    );
    assert!(
        checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "-0.5")]).is_err(),
        "负比率"
    );

    println!("[M3-18] 数值上限：NUMERIC(18,4)/(18,6) 恰好 / 越界两侧、百分比 100 vs 101、负数全拒 ✓");
}

/// 百分比只校验**整数部分**（`100.999999` 会通过）：钉住现状，改口径时这条会红。
///
/// 这是 M3-15 实现留下的松紧度（`value.trunc()` 比较），不是本卡引入的。
/// 之所以写进单测而不是「顺手改成 100.000001 就拒」：本卡只做单测，改行为属于政策口径变更，
/// 要动的是 `checked_unit` 的上限判定与 V0438 的 CHECK（两处必须一起改）。留断言 = 留发现点。
#[test]
fn percent_limit_checks_integer_part_only() {
    let passed = checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[tier("0", None, "100.999999")]);
    assert!(
        passed.is_ok(),
        "现状是「整数部分 ≤ 100 即通过」，若这里失败说明上限判定已改为整体比较 —— 请同步删掉本断言的注释"
    );
    assert!(
        checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[tier("0", None, "101")]).is_err(),
        "101（整数部分 > 100）必须拒"
    );
    println!("[M3-18] 百分比上限按整数部分判定：100.999999 通过、101 拒（现状留痕）✓");
}

// ==================== 段 3：落档边界（左闭右开） ====================

/// 每个档位边界的 **-0.0001 / 恰好 / +0.0001** 三侧都测：边界值归**下一档**（左闭右开）。
#[test]
fn pick_tier_edges_are_half_open() {
    let rules = rules_of(RULE_TIERED, UNIT_PER_UNIT, &three_tiers());

    struct Case {
        value: &'static str,
        want: Option<i16>,
        why: &'static str,
    }
    let cases = [
        Case {
            value: "-100",
            want: None,
            why: "负数没有档位（不 panic、不落首档）",
        },
        Case {
            value: "-0.0001",
            want: None,
            why: "负数没有档位",
        },
        Case {
            value: "0",
            want: Some(1),
            why: "首档下限（闭）",
        },
        Case {
            value: "0.0001",
            want: Some(1),
            why: "首档下限 +1 分",
        },
        Case {
            value: "99.9999",
            want: Some(1),
            why: "第 1 档上限 -0.0001",
        },
        Case {
            value: "100",
            want: Some(2),
            why: "第 1 档上限（开）→ 归第 2 档",
        },
        Case {
            value: "100.0001",
            want: Some(2),
            why: "第 2 档下限 +0.0001",
        },
        Case {
            value: "499.9999",
            want: Some(2),
            why: "第 2 档上限 -0.0001",
        },
        Case {
            value: "500",
            want: Some(3),
            why: "第 2 档上限（开）→ 归第 3 档",
        },
        Case {
            value: "500.0001",
            want: Some(3),
            why: "末档下限 +0.0001",
        },
        Case {
            value: "99999999999999.9999",
            want: Some(3),
            why: "末档无上限（NUMERIC 上限也落得下）",
        },
    ];

    for case in &cases {
        let actual = pick_tier(&rules, dec(case.value)).map(|rule| rule.seq);
        assert_eq!(actual, case.want, "值 {} 落档不对：{}", case.value, case.why);
    }

    assert!(pick_tier(&[], dec("1")).is_none(), "空档位表必须返回 None 而不是 panic");
    println!(
        "[M3-18] 落档边界 {} 例：每个档位边界的 ±0.0001 三侧、负数 None、末档开口 ✓",
        cases.len()
    );
}

/// 落档只看区间，不看类型：阶梯「按额分档」时判据是销售额而不是销量。
#[test]
fn pick_tier_uses_the_base_matching_the_unit() {
    // 同一份两档规则，销量 1 件 / 销售额 12000 元：按量落第 1 档，按额落第 2 档
    let tiers = vec![tier("0", Some("10000"), "1"), tier("10000", None, "3")];
    let by_qty = calc_rebate(
        RULE_TIERED,
        UNIT_PER_UNIT,
        &rules_of(RULE_TIERED, UNIT_PER_UNIT, &tiers),
        &BaseNumbers {
            order_count: 1,
            order_amount: dec("12000"),
            order_qty: dec("1"),
            ..Default::default()
        },
    )
    .expect("按量分档");
    assert_eq!(by_qty.tier.seq, 1, "按量分档看销量（1 件）");
    assert_eq!(by_qty.rebate_amount, dec("1.00"), "1 件 × 1 元/件");

    let by_amount = calc_rebate(
        RULE_TIERED,
        UNIT_PERCENT,
        &rules_of(RULE_TIERED, UNIT_PERCENT, &tiers),
        &BaseNumbers {
            order_count: 1,
            order_amount: dec("12000"),
            order_qty: dec("1"),
            ..Default::default()
        },
    )
    .expect("按额分档");
    assert_eq!(by_amount.tier.seq, 2, "按额分档看销售额（12000 元）");
    assert_eq!(by_amount.rebate_amount, dec("360.00"), "12000 × 3%");
    println!("[M3-18] 同一份档位：按量看销量落 1 档、按额看销售额落 2 档 ✓");
}

// ==================== 段 4：三种规则的计价（边界基数） ====================

/// 三种规则 × 边界基数：应返金额、落档、`pick_value` 三件套逐格对齐。
#[test]
fn calc_matrix_over_boundary_bases() {
    struct Case {
        name: &'static str,
        rule_type: &'static str,
        unit: &'static str,
        tiers: Vec<RuleInput>,
        base: BaseNumbers,
        want_tier: i16,
        want_pick: &'static str,
        want_amount: &'static str,
    }

    let flat_qty = vec![tier("0", None, "0.5")];
    let flat_amount = vec![tier("0", None, "2.5")];
    let cases = vec![
        Case {
            name: "按量·基数 0",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: flat_qty.clone(),
            base: base_qty("0"),
            want_tier: 1,
            want_pick: "0",
            want_amount: "0.00",
        },
        Case {
            name: "按量·1 件",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: flat_qty.clone(),
            base: base_qty("1"),
            want_tier: 1,
            want_pick: "1",
            want_amount: "0.50",
        },
        Case {
            name: "按量·小数件 0.333（分位向远零进位）",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: flat_qty.clone(),
            base: base_qty("0.333"),
            want_tier: 1,
            want_pick: "0.333",
            want_amount: "0.17",
        },
        Case {
            name: "按量·120 件（净额含退货）",
            rule_type: RULE_BY_QTY,
            unit: UNIT_PER_UNIT,
            tiers: flat_qty.clone(),
            base: BaseNumbers {
                order_count: 2,
                order_amount: dec("1000"),
                order_qty: dec("120"),
                return_count: 1,
                return_amount: dec("100"),
                return_qty: dec("20"),
            },
            want_tier: 1,
            want_pick: "100",
            want_amount: "50.00",
        },
        Case {
            name: "按额·基数 0",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: flat_amount.clone(),
            base: base_amount("0"),
            want_tier: 1,
            want_pick: "0",
            want_amount: "0.00",
        },
        Case {
            name: "按额·1 元（0.025 半分，银行家舍入 → 0.02）",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: flat_amount.clone(),
            base: base_amount("1"),
            want_tier: 1,
            want_pick: "1",
            want_amount: "0.02",
        },
        Case {
            name: "按额·1130 元",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: flat_amount.clone(),
            base: base_amount("1130"),
            want_tier: 1,
            want_pick: "1130",
            want_amount: "28.25",
        },
        Case {
            name: "按额·100% 全返",
            rule_type: RULE_BY_AMOUNT,
            unit: UNIT_PERCENT,
            tiers: vec![tier("0", None, "100")],
            base: base_amount("1234.56"),
            want_tier: 1,
            want_pick: "1234.56",
            want_amount: "1234.56",
        },
        Case {
            name: "阶梯按量·99.9999（1 档上沿）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: three_tiers(),
            base: base_qty("99.9999"),
            want_tier: 1,
            want_pick: "99.9999",
            want_amount: "50.00",
        },
        Case {
            name: "阶梯按量·100（正好跳到 2 档，且不累进）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: three_tiers(),
            base: base_qty("100"),
            want_tier: 2,
            want_pick: "100",
            want_amount: "100.00",
        },
        Case {
            name: "阶梯按量·150（2 档全额乘，不是 100×0.5+50×1）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: three_tiers(),
            base: base_qty("150"),
            want_tier: 2,
            want_pick: "150",
            want_amount: "150.00",
        },
        Case {
            name: "阶梯按量·500（跳到末档）",
            rule_type: RULE_TIERED,
            unit: UNIT_PER_UNIT,
            tiers: three_tiers(),
            base: base_qty("500"),
            want_tier: 3,
            want_pick: "500",
            want_amount: "1000.00",
        },
        Case {
            name: "阶梯按额·9999.99（1 档上沿，分位进位到 100.00）",
            rule_type: RULE_TIERED,
            unit: UNIT_PERCENT,
            tiers: vec![tier("0", Some("10000"), "1"), tier("10000", None, "3")],
            base: base_amount("9999.99"),
            want_tier: 1,
            want_pick: "9999.99",
            want_amount: "100.00",
        },
        Case {
            name: "阶梯按额·10000（恰好跳 2 档）",
            rule_type: RULE_TIERED,
            unit: UNIT_PERCENT,
            tiers: vec![tier("0", Some("10000"), "1"), tier("10000", None, "3")],
            base: base_amount("10000"),
            want_tier: 2,
            want_pick: "10000",
            want_amount: "300.00",
        },
    ];

    for case in &cases {
        let rules = rules_of(case.rule_type, case.unit, &case.tiers);
        let outcome = calc_rebate(case.rule_type, case.unit, &rules, &case.base)
            .unwrap_or_else(|error| panic!("【{}】不该报错：{}", case.name, error.msg));
        assert_eq!(outcome.tier.seq, case.want_tier, "【{}】落档不对", case.name);
        assert_eq!(outcome.pick_value, dec(case.want_pick), "【{}】落档基数不对", case.name);
        assert_eq!(
            outcome.rebate_amount,
            dec(case.want_amount),
            "【{}】应返金额不对",
            case.name
        );
        assert!(outcome.rebate_amount.scale() <= 2, "【{}】精度不得多于 2 位", case.name);
        assert_eq!(
            money2(outcome.rebate_amount),
            outcome.rebate_amount,
            "【{}】money2 幂等",
            case.name
        );
        assert!(
            outcome.rebate_amount >= Decimal::ZERO,
            "【{}】应返金额恒 ≥ 0",
            case.name
        );
        assert!(
            outcome.warnings.is_empty(),
            "【{}】正常期间不该有提示：{:?}",
            case.name,
            outcome.warnings
        );
    }
    println!(
        "[M3-18] 计价矩阵 {} 例：三种规则 × 边界基数（0 / 边界 / 边界±ε / 100% / 不累进）✓",
        cases.len()
    );
}

// ==================== 段 5：分位（半分怎么舍） ====================

/// **半分策略 = 银行家舍入（四舍六入五取偶）**，不是「五就进位」。
///
/// 这是 `rust_decimal 1.43` 的 `round_dp` 默认行为（`MidpointNearestEven`），
/// `rebate_settle_dto::money2` 直接用它，所以整条返利链（结算 / 红字发票金额 / 凭证行）
/// 都是这个口径。实测样本：
///
/// ```text
/// 0.005 → 0.00    0.015 → 0.02    0.025 → 0.02    0.035 → 0.04    0.045 → 0.04
/// 0.125 → 0.12    0.135 → 0.14    0.145 → 0.14    1.005 → 1.00    0.9995 → 1.00
/// ```
///
/// 也就是「正好半分时往**偶数**那边靠」：0.005 的第 2 位 0 是偶数 ⇒ 保住 0.00；
/// 0.025 的第 2 位 2 是偶数 ⇒ 也保住 0.02（**不是** 0.03）。
///
/// # 为什么用真表钉住，而不是改成「五就进位」
///
/// 改舍入策略是**会计口径决定**（半分钱的归属、与财务手工表的对账方式），不是单测能拍板的：
/// 改成 `MidpointAwayFromZero` 要同时看结算金额、红字发票 `split_invoice` 的分位恒等、
/// 凭证借贷平衡三处。本卡只做单测，所以把**现状**逐格钉死 —— 将来谁改口径，这张表会立刻红，
/// 改的人必须连带更新这里的期望值（而不是悄悄变了没人发现）。
#[test]
fn rounding_strategy_is_bankers_half_even() {
    // (说明, 规则, 单位, 基数, 比率, 期望金额)
    let cases = [
        (
            "按量 1 × 0.005（半分，第 2 位 0 偶 → 舍）",
            RULE_BY_QTY,
            UNIT_PER_UNIT,
            "1",
            "0.005",
            "0.00",
        ),
        (
            "按量 1 × 0.006（过半分 → 进）",
            RULE_BY_QTY,
            UNIT_PER_UNIT,
            "1",
            "0.006",
            "0.01",
        ),
        (
            "按量 1 × 0.004（不到半分 → 舍）",
            RULE_BY_QTY,
            UNIT_PER_UNIT,
            "1",
            "0.004",
            "0.00",
        ),
        (
            "按量 3 × 0.3333 = 0.9999（进位）",
            RULE_BY_QTY,
            UNIT_PER_UNIT,
            "3",
            "0.3333",
            "1.00",
        ),
        (
            "按额 100 × 0.015% = 0.015（半分，1 奇 → 进）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "100",
            "0.015",
            "0.02",
        ),
        (
            "按额 100 × 0.025% = 0.025（半分，2 偶 → 舍）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "100",
            "0.025",
            "0.02",
        ),
        (
            "按额 100 × 0.035% = 0.035（半分，3 奇 → 进）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "100",
            "0.035",
            "0.04",
        ),
        (
            "按额 100 × 0.014% = 0.014（不到半分 → 舍）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "100",
            "0.014",
            "0.01",
        ),
        (
            "按额 100 × 0.016% = 0.016（过半分 → 进）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "100",
            "0.016",
            "0.02",
        ),
        (
            "按额 3 × 10% = 0.30（整分不动）",
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            "3",
            "10",
            "0.30",
        ),
    ];

    for (name, rule_type, unit, base_text, rate, want) in cases {
        let base = if unit == UNIT_PER_UNIT {
            base_qty(base_text)
        } else {
            base_amount(base_text)
        };
        let outcome = calc_rebate(
            rule_type,
            unit,
            &rules_of(rule_type, unit, &[tier("0", None, rate)]),
            &base,
        )
        .unwrap_or_else(|error| panic!("【{name}】不该报错：{}", error.msg));
        assert_eq!(
            outcome.rebate_amount,
            dec(want),
            "【{name}】{base_text} × {rate} 应为 {want}"
        );
        assert!(outcome.rebate_amount.scale() <= 2, "【{name}】精度不得多于 2 位");
        // 幂等：已算好的金额再 money2 一次不变（落库 NUMERIC(18,4)、再读回来算不会漂）
        assert_eq!(
            money2(outcome.rebate_amount),
            outcome.rebate_amount,
            "【{name}】money2 必须幂等"
        );
    }
    println!(
        "[M3-18] 分位 {} 例：现状是银行家舍入（0.005→0.00 / 0.025→0.02 / 0.035→0.04），逐格钉死 ✓",
        cases.len()
    );
}

// ==================== 段 6：空期间 / 纯退货 / 净额恰好 0 ====================

/// 三种「没有正数基数」的情形：金额都是 0，但**提示条数与文案不同**（不能都当成一种）。
#[test]
fn empty_negative_and_zero_net_bases() {
    let rules = rules_of(RULE_BY_QTY, UNIT_PER_UNIT, &[tier("0", None, "0.5")]);

    // ① 空期间：无单无退 → 1 条提示（没有已确认销售订单）
    let empty = calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &rules, &BaseNumbers::default()).expect("空期间能算");
    assert_eq!(empty.rebate_amount, dec("0.00"), "空期间应返 0");
    assert_eq!(empty.pick_value, Decimal::ZERO);
    assert_eq!(empty.warnings.len(), 1, "空期间恰好 1 条提示：{:?}", empty.warnings);
    assert!(empty.warnings[0].contains("没有已确认销售订单"), "{:?}", empty.warnings);

    // ② 纯退货（有退货、无销售）：净额为负 → 1 条提示（退货多于销售），没有「无订单」那条
    let pure_return = calc_rebate(
        RULE_BY_QTY,
        UNIT_PER_UNIT,
        &rules,
        &BaseNumbers {
            order_count: 0,
            order_qty: Decimal::ZERO,
            return_count: 1,
            return_amount: dec("36"),
            return_qty: dec("12"),
            ..Default::default()
        },
    )
    .expect("纯退货能算");
    assert_eq!(pure_return.rebate_amount, dec("0.00"), "纯退货不产生负返利");
    assert_eq!(pure_return.pick_value, Decimal::ZERO, "落档基数按 0");
    assert_eq!(
        pure_return.warnings.len(),
        1,
        "纯退货只该有「退货多于销售」1 条：{:?}",
        pure_return.warnings
    );
    assert!(
        pure_return.warnings[0].contains("退货多于销售"),
        "{:?}",
        pure_return.warnings
    );

    // ③ 净额恰好 0（有单有退、数量与金额抵平）：不是负数 → 0 条提示，金额 0
    let exact_zero = calc_rebate(
        RULE_BY_QTY,
        UNIT_PER_UNIT,
        &rules,
        &BaseNumbers {
            order_count: 1,
            order_amount: dec("100"),
            order_qty: dec("10"),
            return_count: 1,
            return_amount: dec("100"),
            return_qty: dec("10"),
        },
    )
    .expect("净额 0 能算");
    assert_eq!(exact_zero.pick_value, Decimal::ZERO);
    assert_eq!(exact_zero.rebate_amount, dec("0.00"));
    assert!(
        exact_zero.warnings.is_empty(),
        "净额恰好 0 不该报警：{:?}",
        exact_zero.warnings
    );

    // ④ 负基数下三种类型行为一致（都按 0 落档，不产生负返利）
    for (rule_type, unit, tiers) in [
        (RULE_BY_QTY, UNIT_PER_UNIT, vec![tier("0", None, "0.5")]),
        (RULE_BY_AMOUNT, UNIT_PERCENT, vec![tier("0", None, "5")]),
        (RULE_TIERED, UNIT_PER_UNIT, three_tiers()),
    ] {
        let outcome = calc_rebate(
            rule_type,
            unit,
            &rules_of(rule_type, unit, &tiers),
            &BaseNumbers {
                order_count: 1,
                order_amount: dec("10"),
                order_qty: dec("1"),
                return_count: 1,
                return_amount: dec("500"),
                return_qty: dec("50"),
            },
        )
        .expect("负基数也要能算");
        assert_eq!(outcome.pick_value, Decimal::ZERO, "{rule_type} 负基数落档按 0");
        assert_eq!(outcome.rebate_amount, dec("0.00"), "{rule_type} 负基数应返 0");
        assert_eq!(outcome.tier.seq, 1, "{rule_type} 负基数落首档（不是 None）");
    }
    println!("[M3-18] 空期间 / 纯退货 / 净额恰好 0 / 三类型负基数：金额恒 0、提示条数各不相同 ✓");
}

// ==================== 段 7：线上路径（DTO → 校验 → 计价） ====================

/// 走 handler 的同一条路：`RebateRuleReq::to_input` → `checked_rules` → `calc_rebate`。
///
/// 这条用例的价值是「接口收的形态」也被覆盖：`tier_from` 省略默认 0、`rate` 省略 400 点名档位、
/// 备注 trim、乱序入参重排 —— 三条都是 handler 里 `rule_inputs()` + `checked_rules()` 的实际行为。
#[test]
fn wire_path_from_rule_req_to_amount() {
    let req = |from: Option<&str>, to: Option<&str>, rate: Option<&str>, remark: Option<&str>| RebateRuleReq {
        tier_from: from.map(dec),
        tier_to: to.map(dec),
        rate: rate.map(dec),
        remark: remark.map(str::to_string),
    };

    // ① 按量：省略 tier_from（默认 0）、备注带空白（trim）
    let by_qty_req = [req(None, None, Some("0.5"), Some("  标准返点  "))];
    let inputs = by_qty_req
        .iter()
        .enumerate()
        .map(|(index, rule)| rule.to_input(index))
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("DTO 转输入");
    assert_eq!(inputs[0].tier_from, Decimal::ZERO, "省略 tier_from 默认 0");
    assert_eq!(inputs[0].remark.as_deref(), Some("标准返点"), "备注要 trim");
    let rules = checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &inputs).expect("按量合法");
    let outcome = calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &rules, &base_qty("200")).expect("算");
    assert_eq!(outcome.rebate_amount, dec("100.00"), "200 件 × 0.5");

    // ② 按额：省略 rate_unit（默认百分比）
    let by_amount_req = [req(Some("0"), None, Some("3"), None)];
    let unit = checked_unit(RULE_BY_AMOUNT, None).expect("按额默认百分比");
    assert_eq!(unit, UNIT_PERCENT);
    let inputs = by_amount_req
        .iter()
        .enumerate()
        .map(|(index, rule)| rule.to_input(index))
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let rules = checked_rules(RULE_BY_AMOUNT, &unit, &inputs).expect("按额合法");
    let outcome = calc_rebate(RULE_BY_AMOUNT, &unit, &rules, &base_amount("10000")).expect("算");
    assert_eq!(outcome.rebate_amount, dec("300.00"), "10000 × 3%");

    // ③ 阶梯：乱序入参（先给末档）、显式单位
    let tiered_req = [
        req(Some("500"), None, Some("2"), None),
        req(Some("0"), Some("100"), Some("0.5"), None),
        req(Some("100"), Some("500"), Some("1"), None),
    ];
    let unit = checked_unit(RULE_TIERED, Some("per_unit")).expect("阶梯显式单位（小写归一）");
    assert_eq!(unit, UNIT_PER_UNIT);
    let inputs = tiered_req
        .iter()
        .enumerate()
        .map(|(index, rule)| rule.to_input(index))
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let rules = checked_rules(RULE_TIERED, &unit, &inputs).expect("乱序合法");
    assert_eq!(
        rules.iter().map(|rule| rule.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "重排后编号 1..3"
    );
    let outcome = calc_rebate(RULE_TIERED, &unit, &rules, &base_qty("120")).expect("算");
    assert_eq!(outcome.tier.seq, 2, "120 落第 2 档");
    assert_eq!(outcome.rebate_amount, dec("120.00"), "120 × 1（2 档全额乘）");

    // ④ 缺 rate：400 + 66089 且点名「第 2 档」
    let missing = req(Some("0"), None, None, None)
        .to_input(1)
        .expect_err("缺 rate 必须拒");
    assert_eq!(missing.code, error_code::ERP_REBATE_RULE_INVALID);
    assert!(missing.msg.contains("第 2 档"), "{}", missing.msg);

    println!("[M3-18] 线上路径：三种规则各走一遍 DTO→checked_rules→calc_rebate，与 handler 同一组函数 ✓");
}

// ==================== 段 8：类型 × 单位 组合配对 ====================

/// 3 类型 × 2 单位 = 6 组合，只有 4 组能算：`BY_QTY+PER_UNIT` / `BY_AMOUNT+PERCENT` /
/// `TIERED+PER_UNIT` / `TIERED+PERCENT`；其余 2 组 400 + 66089（不静默按其中一边算）。
#[test]
fn type_unit_pairing_matrix() {
    let mut ok_count = 0;
    let mut err_count = 0;
    for rule_type in RULE_TYPES {
        for unit in RATE_UNITS {
            let tiers = if rule_type == RULE_TIERED {
                three_tiers()
            } else {
                vec![tier("0", None, "1")]
            };
            let result = calc_rebate(rule_type, unit, &rules_of(rule_type, unit, &tiers), &base_amount("100"));
            let should_ok = !((rule_type == RULE_BY_QTY && unit == UNIT_PERCENT)
                || (rule_type == RULE_BY_AMOUNT && unit == UNIT_PER_UNIT));
            match (should_ok, result) {
                (true, Ok(_)) => ok_count += 1,
                (false, Err(error)) => {
                    err_count += 1;
                    assert_eq!(error.status, 400, "{rule_type}+{unit}");
                    assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID, "{rule_type}+{unit}");
                }
                (true, Err(error)) => panic!("{rule_type}+{unit} 应当能算：{}", error.msg),
                (false, Ok(_)) => panic!("{rule_type}+{unit} 必须拒（量纲不同不能相乘）"),
            }
        }
    }
    assert_eq!((ok_count, err_count), (4, 2), "6 组合 = 4 能算 + 2 拒");

    // 政策的默认单位：按量→元/件、按额→百分比、阶梯→必须显式
    assert_eq!(checked_unit(RULE_BY_QTY, None).expect("按量默认"), UNIT_PER_UNIT);
    assert_eq!(checked_unit(RULE_BY_AMOUNT, None).expect("按额默认"), UNIT_PERCENT);
    assert!(checked_unit(RULE_TIERED, None).is_err(), "阶梯不给单位必须 400");
    println!("[M3-18] 类型×单位 6 组合：4 能算 + 2 拒（量纲不同不静默相乘）✓");
}

// ==================== 段 9：不变量（跨基数网格） ====================

/// 不变量：应返恒 ≥ 0、恒 2 位、落档基数 = max(净额, 0)、基数递增时（费率不递减的）应返不递减。
#[test]
fn invariants_hold_across_base_grid() {
    let ladders = [
        (RULE_BY_QTY, UNIT_PER_UNIT, vec![tier("0", None, "0.5")]),
        (RULE_BY_AMOUNT, UNIT_PERCENT, vec![tier("0", None, "2.5")]),
        (RULE_TIERED, UNIT_PER_UNIT, three_tiers()),
    ];
    let grid = [
        "-100",
        "-0.01",
        "0",
        "0.01",
        "50",
        "99.9999",
        "100",
        "100.0001",
        "499.9999",
        "500",
        "1000",
        "99999999.9999",
    ];

    for (rule_type, unit, tiers) in &ladders {
        let rules = rules_of(rule_type, unit, tiers);
        let mut previous = Decimal::ZERO;
        for text in grid {
            let value = dec(text);
            let base = if *unit == UNIT_PER_UNIT {
                base_qty(text)
            } else {
                base_amount(text)
            };
            let outcome = calc_rebate(rule_type, unit, &rules, &base).expect("网格内必须能算");
            let floor = value.max(Decimal::ZERO);
            assert_eq!(
                outcome.pick_value, floor,
                "{rule_type} 基数 {text}：落档基数应为 max(净额,0)"
            );
            assert!(
                outcome.rebate_amount >= Decimal::ZERO,
                "{rule_type} 基数 {text}：应返不能为负"
            );
            assert!(
                outcome.rebate_amount.scale() <= 2,
                "{rule_type} 基数 {text}：精度不得多于 2 位"
            );
            assert_eq!(
                money2(outcome.rebate_amount),
                outcome.rebate_amount,
                "{rule_type} 基数 {text}：money2 必须幂等（落库/读回不漂）"
            );
            assert!(
                outcome.rebate_amount >= previous,
                "{rule_type} 基数 {text}：费率不递减时，基数涨了应返不能跌（上一格 {previous}）"
            );
            previous = outcome.rebate_amount;
        }
    }
    println!(
        "[M3-18] 不变量网格：3 规则 × {} 个基数（负数/0/边界/边界±ε/超大）恒 ≥0、2 位、单调 ✓",
        grid.len()
    );
}

// ==================== 段 10：结算期间边界 ====================

/// 月度 / 季度的起止是**左闭右开**：跨年、闰年 2 月、季末都按真实日历算。
#[test]
fn settlement_period_boundaries() {
    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    struct Case {
        period: &'static str,
        code: &'static str,
        from: &'static str,
        to: &'static str,
        last_day: &'static str,
        why: &'static str,
    }
    let cases = [
        Case {
            period: "MONTH",
            code: "2026-11",
            from: "2026-11-01",
            to: "2026-12-01",
            last_day: "2026-11-30",
            why: "30 天的月",
        },
        Case {
            period: "MONTH",
            code: "2026-12",
            from: "2026-12-01",
            to: "2027-01-01",
            last_day: "2026-12-31",
            why: "跨年月",
        },
        Case {
            period: "MONTH",
            code: "2028-02",
            from: "2028-02-01",
            to: "2028-03-01",
            last_day: "2028-02-29",
            why: "闰年 2 月（29 天）",
        },
        Case {
            period: "MONTH",
            code: "2026-02",
            from: "2026-02-01",
            to: "2026-03-01",
            last_day: "2026-02-28",
            why: "平年 2 月（28 天）",
        },
        Case {
            period: "QUARTER",
            code: "2026-Q1",
            from: "2026-01-01",
            to: "2026-04-01",
            last_day: "2026-03-31",
            why: "Q1 末日 3-31（不是 4-1）",
        },
        Case {
            period: "QUARTER",
            code: "2026-Q4",
            from: "2026-10-01",
            to: "2027-01-01",
            last_day: "2026-12-31",
            why: "跨年季度",
        },
        Case {
            period: "QUARTER",
            code: "2028-Q1",
            from: "2028-01-01",
            to: "2028-04-01",
            last_day: "2028-03-31",
            why: "含闰日的季度",
        },
    ];

    for case in &cases {
        let resolved = PeriodArgs::resolve(case.period, case.code)
            .unwrap_or_else(|error| panic!("【{}】{} 应能解析：{}", case.why, case.code, error.msg));
        assert_eq!(resolved.from, day(case.from), "【{}】期间首日（含）", case.why);
        assert_eq!(resolved.to, day(case.to), "【{}】期间末日 +1（不含）", case.why);
        assert_eq!(resolved.last_day(), day(case.last_day), "【{}】末日（含）", case.why);
        assert_eq!(resolved.settle_period, case.period);
    }

    // 形状不互配：月度配季度码、季度配月度码，都 400
    assert_eq!(
        PeriodArgs::resolve("MONTH", "2026-Q4")
            .expect_err("月度配季度码")
            .status,
        400
    );
    assert_eq!(
        PeriodArgs::resolve("QUARTER", "2026-11")
            .expect_err("季度配月度码")
            .status,
        400
    );

    // 结算单状态：三个值，终态语义是「已开红字发票」（M3-17 定稿，改口径时这条会红）
    assert_eq!(STATUS_DRAFT, "DRAFT");
    assert_eq!(STATUS_SETTLED, "SETTLED");
    assert_eq!(super::rebate_settle_dto::status_label(STATUS_SETTLED), "已开红字发票");

    println!(
        "[M3-18] 期间边界 {} 例：月度/季度左闭右开、跨年、闰年 2 月、季末 ✓",
        cases.len()
    );
}

// ==================== 段 11：20 档上限的端起端落 ====================

/// 20 档（上限）不只是「存得下」，还要**首档与末档都落得对**。
#[test]
fn max_tier_ladder_works_end_to_end() {
    let tiers: Vec<RuleInput> = (0..MAX_TIERS)
        .map(|index| {
            let from = index * 10;
            RuleInput {
                tier_from: Decimal::from(from),
                tier_to: if index + 1 == MAX_TIERS {
                    None
                } else {
                    Some(Decimal::from((index + 1) * 10))
                },
                rate: dec("1"),
                remark: None,
            }
        })
        .collect();
    let rules = rules_of(RULE_TIERED, UNIT_PER_UNIT, &tiers);
    assert_eq!(rules.len(), MAX_TIERS);
    assert_eq!(rules[0].seq, 1);
    assert_eq!(rules[MAX_TIERS - 1].seq, i16::try_from(MAX_TIERS).unwrap());

    for (value, want_seq, want_amount) in [
        ("0", 1_i16, "0.00"),
        ("9.9999", 1, "10.00"),
        ("10", 2, "10.00"),
        ("190", 20, "190.00"),
        ("1000", 20, "1000.00"),
    ] {
        let outcome = calc_rebate(RULE_TIERED, UNIT_PER_UNIT, &rules, &base_qty(value)).expect("20 档能算");
        assert_eq!(outcome.tier.seq, want_seq, "值 {value} 落档不对");
        assert_eq!(outcome.rebate_amount, dec(want_amount), "值 {value} 金额不对");
    }
    assert_eq!(MIN_TIER_TIERS, 2, "阶梯最少 2 档（1 档请用按量/按额）");
    assert_eq!(rebate_dto::rule_type_label(RULE_TIERED), "阶梯");
    println!("[M3-18] 20 档上限：首档 0 / 中间档 10 / 末档 190 与超上限全部落对 ✓");
}
