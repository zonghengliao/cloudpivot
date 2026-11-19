//! 成本还原「还原后报表：料工费占比」（M5-28）的**纯逻辑单测**（不连库）。
//!
//! 钉住的口径（真库用例 `cost_restore_report_db_tests` 再验数据面）：
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`dept_groups_merge_rows_and_shares_use_before_total`] | 同车间合并、占比分母 = 还原前合计、四项合计 100.00 |
//! | 2 | [`unresolved_is_its_own_slice`] | 还原不彻底单列一格（不摊进三要素），四项仍 100.00（±0.01 舍入差） |
//! | 3 | [`shift_is_after_minus_before_in_points`] | 占比变化 pp = 还原后 − 还原前；三要素 pp 之和 = −不彻底占比 |
//! | 4 | [`group_by_variants_produce_expected_keys`] | 四档维度的 key / label / 副标 |
//! | 5 | [`object_group_equals_its_row`] | 按对象分组 = 明细表的一行（金额一分不差） |
//! | 6 | [`empty_rows_give_zero_totals`] | 空表：合计 0、占比 None（不画假 0.00%）、主导要素 none |
//! | 7 | [`groups_are_sorted_by_restored_total_desc`] | 排序确定（还原后合计降序 → key 升序），翻页不抖 |
//! | 8 | [`totals_equal_the_sum_of_groups`] | 整表合计 = 各分组之和（金额 / 行数 / 分档行数） |
//! | 9 | [`dominant_picks_the_largest_amount`] | 主导要素按金额比（不被 2 位舍入带偏） |
//! | 10 | [`semi_share_is_over_material_input`] | 半成品占比的分母是**材料投入**（不是总额） |
//! | 11 | [`check_surfaces_an_unbalanced_row`] | 守恒自检：不守恒时 check ≠ 0（不静默） |

use nik::prelude::rust_decimal::Decimal;

use crate::business::erp::cost::cost_restore_report_dto::{
    Dominant, ElementView, GroupBy, Row, aggregate, dominant_of, group_view, key_of, money_text, totals_acc,
};
use crate::business::erp::cost::cost_restore_rollup_dto::DeltaView;

/// 金额字面量。
fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or_else(|_| panic!("数值字面量：{text}"))
}

/// 三要素 → 出参（合计自己加，和 M5-27 的 `ElementSplit::view` 同口径）。
fn element(material: &str, labor: &str, overhead: &str) -> ElementView {
    let total = dec(material) + dec(labor) + dec(overhead);
    ElementView {
        material: money_text(dec(material)),
        labor: money_text(dec(labor)),
        overhead: money_text(dec(overhead)),
        total: money_text(total),
    }
}

/// 造一行（形状与 M5-27 的出参一致：`after.total + unresolved = before.total`）。
struct Draft<'a> {
    object: &'a str,
    dept: (&'a str, &'a str),
    product: (&'a str, &'a str, &'a str),
    lot: Option<&'a str>,
    before: (&'a str, &'a str, &'a str),
    after: (&'a str, &'a str, &'a str),
    semi: &'a str,
    unresolved: &'a str,
    state: &'a str,
}

impl<'a> Draft<'a> {
    fn row(&self) -> Row {
        let before = element(self.before.0, self.before.1, self.before.2);
        let after = element(self.after.0, self.after.1, self.after.2);
        let material_delta = dec(&after.material) - dec(&before.material);
        let labor_delta = dec(&after.labor) - dec(&before.labor);
        let overhead_delta = dec(&after.overhead) - dec(&before.overhead);
        Row {
            object_id: format!("obj-{}", self.object),
            object_no: self.object.to_string(),
            object_type: "BATCH".to_string(),
            lot_no: self.lot.map(str::to_string),
            product_id: self.product.0.to_string(),
            product_code: Some(self.product.1.to_string()),
            product_name: Some(self.product.2.to_string()),
            workshop_dept_id: self.dept.0.to_string(),
            workshop_dept_name: Some(self.dept.1.to_string()),
            item: None,
            finished_cost: None,
            semi_in_material: money_text(dec(self.semi)),
            purchased_in_material: money_text(dec(&before.material) - dec(self.semi)),
            semi_share: None,
            before,
            after,
            after_material_share: None,
            after_labor_share: None,
            after_overhead_share: None,
            delta: DeltaView {
                material: money_text(material_delta),
                labor: money_text(labor_delta),
                overhead: money_text(overhead_delta),
                total: money_text(material_delta + labor_delta + overhead_delta),
                note: String::new(),
            },
            restored_labor: money_text(labor_delta),
            restored_overhead: money_text(overhead_delta),
            unresolved: money_text(dec(self.unresolved)),
            state: self.state.to_string(),
            state_label: match self.state {
                "restored" => "已还原",
                "partial" => "还原不彻底",
                _ => "无需还原",
            }
            .to_string(),
            depth: 1,
            node_count: 1,
            flows: Vec::new(),
            semis: Vec::new(),
            issues: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// 一个「材料里有半成品、已还原干净」的成品行：700 = 500/100/100 → 220/196/284。
fn restored_row(object: &str, dept: (&str, &str)) -> Row {
    Draft {
        object,
        dept,
        product: ("prod-f", "FG-001", "成品 F"),
        lot: Some("LOT-01"),
        before: ("500", "100", "100"),
        after: ("220", "196", "284"),
        semi: "400",
        unresolved: "0",
        state: "restored",
    }
    .row()
}

/// 一个全外购料的成品行（还原前后一致）。
fn purchased_row(object: &str, dept: (&str, &str)) -> Row {
    Draft {
        object,
        dept,
        product: ("prod-g", "FG-002", "成品 G"),
        lot: None,
        before: ("300", "30", "20"),
        after: ("300", "30", "20"),
        semi: "0",
        unresolved: "0",
        state: "no_semi",
    }
    .row()
}

// ==================== 用例 ====================

/// ① 同车间两行合并：分母是**还原前合计**，四项（材料 + 人工 + 费 + 不彻底）合计 100.00。
#[test]
fn dept_groups_merge_rows_and_shares_use_before_total() {
    let rows = vec![
        restored_row("CB-1", ("dept-a", "一车间")),
        purchased_row("CB-2", ("dept-a", "一车间")),
        purchased_row("CB-3", ("dept-b", "二车间")),
    ];
    let groups = aggregate(&rows, GroupBy::Dept);
    assert_eq!(groups.len(), 2, "两个车间两组");

    let first = group_view(&groups[0]);
    assert_eq!(first.key, "dept-a");
    assert_eq!(first.label, "一车间");
    assert_eq!(first.object_count, 2);

    // 还原前：材料 500 + 300 = 800、人工 100 + 30 = 130、费 100 + 20 = 120、合计 1050
    assert_eq!(first.before.material, "800");
    assert_eq!(first.before.labor, "130");
    assert_eq!(first.before.overhead, "120");
    assert_eq!(first.before.total, "1050");
    assert_eq!(first.restored.denominator, "1050", "占比分母 = 还原前合计");

    // 还原后：材料 220 + 300 = 520、人工 196 + 30 = 226、费 284 + 20 = 304
    assert_eq!(first.restored.material, "520");
    assert_eq!(first.restored.labor, "226");
    assert_eq!(first.restored.overhead, "304");
    assert_eq!(first.restored.total, "1050");
    assert_eq!(first.restored.unresolved, "0");

    // 占比：520/1050 = 49.5238 → 49.52；226/1050 = 21.5238 → 21.52；304/1050 = 28.9523 → 28.95
    assert_eq!(first.restored.material_share.as_deref(), Some("49.52"));
    assert_eq!(first.restored.labor_share.as_deref(), Some("21.52"));
    assert_eq!(first.restored.overhead_share.as_deref(), Some("28.95"));
    assert_eq!(first.restored.unresolved_share.as_deref(), Some("0.00"));
    assert_eq!(
        first.share_sum.as_deref(),
        Some("99.99"),
        "2 位舍入差暴露在 share_sum 里"
    );

    // 还原前占比：800/1050 = 76.19、130/1050 = 12.38、120/1050 = 11.43
    assert_eq!(first.before.material_share.as_deref(), Some("76.19"));
    assert_eq!(first.before.labor_share.as_deref(), Some("12.38"));
    assert_eq!(first.before.overhead_share.as_deref(), Some("11.43"));

    // 从材料里拆出来的工费：人工 96、费 184
    assert_eq!(first.restored_labor, "96");
    assert_eq!(first.restored_overhead, "184");
    assert_eq!(first.semi_in_material, "400");
    assert_eq!(first.semi_share.as_deref(), Some("50.00"), "半成品占材料投入 400/800");
    assert_eq!(first.restored_rows, 1);
    assert_eq!(first.no_semi_rows, 1);
    assert_eq!(first.partial_rows, 0);
    assert_eq!(first.dominant, "material");
    assert_eq!(first.check, "0");
}

/// ② 还原不彻底是**单独一格**：不摊到材料、也不改人工 / 费用，四项合计仍 100.00。
#[test]
fn unresolved_is_its_own_slice() {
    let row = Draft {
        object: "CB-P",
        dept: ("dept-a", "一车间"),
        product: ("prod-f", "FG-001", "成品 F"),
        lot: Some("LOT-09"),
        before: ("300", "100", "100"),
        after: ("100", "120", "180"),
        semi: "300",
        unresolved: "100",
        state: "partial",
    }
    .row();
    let groups = aggregate(&[row], GroupBy::All);
    assert_eq!(groups.len(), 1);
    let view = group_view(&groups[0]);

    assert_eq!(view.restored.material, "100");
    assert_eq!(view.restored.labor, "120");
    assert_eq!(view.restored.overhead, "180");
    assert_eq!(view.restored.total, "400", "还原后三要素合计（不含不彻底）");
    assert_eq!(view.restored.unresolved, "100");
    assert_eq!(view.restored.denominator, "500", "分母 = 还原后 400 + 不彻底 100");

    // 100/500 = 20.00、120/500 = 24.00、180/500 = 36.00、100/500 = 20.00 ⇒ 正好 100.00
    assert_eq!(view.restored.material_share.as_deref(), Some("20.00"));
    assert_eq!(view.restored.labor_share.as_deref(), Some("24.00"));
    assert_eq!(view.restored.overhead_share.as_deref(), Some("36.00"));
    assert_eq!(view.restored.unresolved_share.as_deref(), Some("20.00"));
    assert_eq!(view.share_sum.as_deref(), Some("100.00"));
    assert_eq!(view.partial_rows, 1);
    assert_eq!(view.check, "0", "守恒：400 + 100 = 500");
}

/// ③ 占比变化（pp）= 还原后 − 还原前；三要素 pp 之和 = − 不彻底占比。
#[test]
fn shift_is_after_minus_before_in_points() {
    let row = restored_row("CB-1", ("dept-a", "一车间"));
    let view = group_view(&aggregate(&[row], GroupBy::All)[0]);

    // 还原前 500/700 = 71.43、100/700 = 14.29、100/700 = 14.29
    assert_eq!(view.before.material_share.as_deref(), Some("71.43"));
    assert_eq!(view.before.labor_share.as_deref(), Some("14.29"));
    // 还原后 220/700 = 31.43、196/700 = 28.00、284/700 = 40.57
    assert_eq!(view.restored.material_share.as_deref(), Some("31.43"));
    assert_eq!(view.restored.labor_share.as_deref(), Some("28.00"));
    assert_eq!(view.restored.overhead_share.as_deref(), Some("40.57"));

    // 31.43 − 71.43 = −40.00；28.00 − 14.29 = +13.71；40.57 − 14.29 = +26.28
    assert_eq!(view.shift.material_pp.as_deref(), Some("-40.00"));
    assert_eq!(view.shift.labor_pp.as_deref(), Some("13.71"));
    assert_eq!(view.shift.overhead_pp.as_deref(), Some("26.28"));
    assert!(!view.shift.note.is_empty(), "pp 的口径文案必须跟着出参走");

    // 三要素 pp 之和（−0.01 舍入）= − 不彻底占比（这里不彻底 0）
    let sum = dec("-40.00") + dec("13.71") + dec("26.28");
    assert!(sum.abs() <= dec("0.01"), "三要素 pp 之和应 ≈ 0，实际 {sum}");
}

/// ④ 四档维度的 key / label / 副标。
#[test]
fn group_by_variants_produce_expected_keys() {
    let row = restored_row("CB-1", ("dept-a", "一车间"));

    let (key, label, hint) = key_of(&row, GroupBy::Dept);
    assert_eq!((key.as_str(), label.as_str(), hint), ("dept-a", "一车间", None));

    let (key, label, hint) = key_of(&row, GroupBy::Product);
    assert_eq!((key.as_str(), label.as_str(), hint), ("prod-f", "FG-001 成品 F", None));

    let (key, label, hint) = key_of(&row, GroupBy::Object);
    assert_eq!((key.as_str(), label.as_str()), ("obj-CB-1", "CB-1"));
    let hint = hint.expect("对象行带副标");
    assert!(hint.contains("FG-001 成品 F"), "{hint}");
    assert!(hint.contains("批号 LOT-01"), "{hint}");

    let (key, label, hint) = key_of(&row, GroupBy::All);
    assert_eq!((key.as_str(), label.as_str(), hint), ("ALL", "全部", None));

    // 车间 / 产出品缺失时不 panic，给出可读兜底
    let mut bare = restored_row("CB-2", ("dept-x", "一车间"));
    bare.workshop_dept_name = None;
    bare.product_code = None;
    bare.product_name = None;
    assert_eq!(key_of(&bare, GroupBy::Dept).1, "未指定车间");
    assert_eq!(key_of(&bare, GroupBy::Product).1, "未指定产出品");
}

/// ⑤ 按对象分组：一对象一组，金额与明细表那一行一分不差。
#[test]
fn object_group_equals_its_row() {
    let rows = vec![
        restored_row("CB-1", ("dept-a", "一车间")),
        purchased_row("CB-2", ("dept-a", "一车间")),
    ];
    let groups = aggregate(&rows, GroupBy::Object);
    assert_eq!(groups.len(), 2, "一对象一组");

    for group in &groups {
        let source = rows
            .iter()
            .find(|row| row.object_no == group.label)
            .unwrap_or_else(|| panic!("报表里出现明细表没有的对象：{}", group.label));
        let view = group_view(group);
        assert_eq!(view.before.material, source.before.material);
        assert_eq!(view.before.total, source.before.total);
        assert_eq!(view.restored.material, source.after.material);
        assert_eq!(view.restored.labor, source.after.labor);
        assert_eq!(view.restored.overhead, source.after.overhead);
        assert_eq!(view.restored.total, source.after.total);
        assert_eq!(view.restored.unresolved, source.unresolved);
        assert_eq!(view.object_count, 1);
        assert_eq!(view.restored.denominator, source.before.total, "分母仍是还原前合计");
    }
}

/// ⑥ 空表：合计全 0，占比给 `None`（不画假 0.00%），主导要素 `none`。
#[test]
fn empty_rows_give_zero_totals() {
    let rows: Vec<Row> = Vec::new();
    assert!(aggregate(&rows, GroupBy::Dept).is_empty());

    let view = group_view(&totals_acc(&rows));
    assert_eq!(view.object_count, 0);
    assert_eq!(view.before.total, "0");
    assert_eq!(view.restored.total, "0");
    assert_eq!(view.restored.denominator, "0");
    assert_eq!(view.before.material_share, None, "分母 0 ⇒ 没有占比，不给 0.00");
    assert_eq!(view.restored.material_share, None);
    assert_eq!(view.restored.unresolved_share, None);
    assert_eq!(view.share_sum, None);
    assert_eq!(view.shift.material_pp, None);
    assert_eq!(view.dominant, "none");
    assert_eq!(view.dominant_label, "无数据");
    assert_eq!(view.check, "0");
}

/// ⑦ 排序确定：还原后合计降序、再按 key 升序（翻页不会每次刷新都变）。
#[test]
fn groups_are_sorted_by_restored_total_desc() {
    let rows = vec![
        purchased_row("CB-1", ("dept-z", "小车间")),
        restored_row("CB-2", ("dept-a", "一车间")),
        purchased_row("CB-3", ("dept-b", "二车间")),
    ];
    let groups = aggregate(&rows, GroupBy::Dept);
    let keys: Vec<&str> = groups.iter().map(|group| group.key.as_str()).collect();
    // 一车间 700 > 二车间 350 > 小车间 350；后两名同额 ⇒ 按 key 升序（dept-b < dept-z）
    assert_eq!(keys, vec!["dept-a", "dept-b", "dept-z"]);

    // 重复调用顺序稳定
    let again = aggregate(&rows, GroupBy::Dept);
    let keys_again: Vec<&str> = again.iter().map(|group| group.key.as_str()).collect();
    assert_eq!(keys, keys_again);
}

/// ⑧ 整表合计 = 各分组之和（金额 / 对象数 / 分档行数）。
#[test]
fn totals_equal_the_sum_of_groups() {
    let rows = vec![
        restored_row("CB-1", ("dept-a", "一车间")),
        purchased_row("CB-2", ("dept-a", "一车间")),
        purchased_row("CB-3", ("dept-b", "二车间")),
    ];
    let totals = totals_acc(&rows);
    let groups = aggregate(&rows, GroupBy::Dept);

    let mut sum = crate::business::erp::cost::cost_restore_report_dto::GroupAcc::default();
    for group in &groups {
        sum.object_count += group.object_count;
        sum.before.add(&group.before);
        sum.before_total += group.before_total;
        sum.after.add(&group.after);
        sum.after_total += group.after_total;
        sum.unresolved += group.unresolved;
        sum.semi_in_material += group.semi_in_material;
        sum.restored_rows += group.restored_rows;
        sum.partial_rows += group.partial_rows;
        sum.no_semi_rows += group.no_semi_rows;
    }
    // key / label 是装配时给的（合计行固定 ALL / 合计），数字才是要验的
    sum.key = totals.key.clone();
    sum.label = totals.label.clone();
    assert_eq!(sum, totals, "合计必须等于分组之和（金额 + 行数 + 分档）");
}

/// ⑨ 主导要素按金额比：谁最大说谁；全 0 说「无数据」；并列时材料优先。
#[test]
fn dominant_picks_the_largest_amount() {
    assert_eq!(dominant_of(dec("520"), dec("226"), dec("304")), Dominant::Material);
    assert_eq!(dominant_of(dec("100"), dec("500"), dec("200")), Dominant::Labor);
    assert_eq!(dominant_of(dec("100"), dec("200"), dec("900")), Dominant::Overhead);
    assert_eq!(dominant_of(Decimal::ZERO, Decimal::ZERO, Decimal::ZERO), Dominant::None);
    assert_eq!(
        dominant_of(dec("300"), dec("300"), dec("100")),
        Dominant::Material,
        "并列材料优先"
    );
    assert_eq!(
        dominant_of(dec("100"), dec("300"), dec("300")),
        Dominant::Labor,
        "并列人工优先"
    );
}

/// ⑩ 半成品占比的分母是**材料投入**（不是总额）：材料 500 里 400 是半成品 ⇒ 80.00%。
#[test]
fn semi_share_is_over_material_input() {
    let view = group_view(&aggregate(&[restored_row("CB-1", ("dept-a", "一车间"))], GroupBy::All)[0]);
    assert_eq!(view.semi_in_material, "400");
    assert_eq!(view.semi_share.as_deref(), Some("80.00"));
    assert_eq!(view.restored_labor, "96");
    assert_eq!(view.restored_overhead, "184");
}

/// ⑪ 守恒自检：造一行不守恒（还原后 + 不彻底 ≠ 还原前），`check` 必须 ≠ 0。
#[test]
fn check_surfaces_an_unbalanced_row() {
    let mut row = restored_row("CB-1", ("dept-a", "一车间"));
    row.unresolved = "50".to_string(); // 硬塞一个不彻底的数：700 + 50 − 700 = 50
    let view = group_view(&aggregate(&[row], GroupBy::All)[0]);
    assert_eq!(view.check, "50", "不守恒要暴露给前端，不能悄悄抹平");
}

/// 一句话的口径自检：`totals` 与「明细表行」是同一批金额（不相加任何东西）。
#[test]
fn totals_match_the_raw_rows() {
    let rows = vec![
        restored_row("CB-1", ("dept-a", "一车间")),
        purchased_row("CB-2", ("dept-b", "二车间")),
    ];
    let view = group_view(&totals_acc(&rows));
    assert_eq!(view.before.total, "1050");
    assert_eq!(view.restored.total, "1050");
    assert_eq!(view.object_count, 2);
    assert_eq!(view.restored_rows, 1);
    assert_eq!(view.no_semi_rows, 1);
    assert_eq!(view.check, "0");
}
