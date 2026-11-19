//! 成本还原「逐层还原到料工费」（M5-27）的**纯逻辑单测**（不连库、不设 `ERP_DB_URL`）。
//!
//! 覆盖：一键卷积（外购料 / 一层半成品 / 三层链）/ 守恒恒等式 / 逐层台账 /
//! 缺结构·截断·闭环 ⇒ 不彻底 / 尾差进制造费用 / 表与结构不合时的不彻底记账 /
//! 三档状态与筛选 / 出参装配。
//!
//! 真库用例在 [`crate::business::erp::cost::cost_restore_rollup_db_tests`]。

use nik::prelude::rust_decimal::Decimal;
use std::str::FromStr;

use crate::business::erp::cost::cost_restore_rollup_dto::{self as dto, ElementSplit, RollupState, StateFilter};
use crate::business::erp::cost::cost_restore_structure_dto::{ElementView, NodeView};

// ==================== 夹具 ====================

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字面量：{text}"))
}

fn elements(material: &str, labor: &str, overhead: &str) -> ElementView {
    let total = dec(material) + dec(labor) + dec(overhead);
    ElementView {
        material: material.to_string(),
        labor: labor.to_string(),
        overhead: overhead.to_string(),
        total: dto::money_text(total),
    }
}

/// 造一个结构节点（`elements = None` ⇒ 结构取不到）。
fn node(
    level: u32,
    code: &str,
    amount: &str,
    split: Option<(&str, &str, &str)>,
    is_semi: bool,
    children: Vec<NodeView>,
) -> NodeView {
    NodeView {
        level,
        material_id: format!("ID-{code}"),
        material_code: Some(code.to_string()),
        material_name: Some(format!("料 {code}")),
        lot_no: Some(format!("LOT-{code}")),
        qty: "1.0000".to_string(),
        unit_cost: None,
        amount: amount.to_string(),
        share_pct: None,
        is_semi,
        source: if is_semi { Some("batch".to_string()) } else { None },
        source_label: if is_semi { Some("按批".to_string()) } else { None },
        producer: None,
        elements: split.map(|(material, labor, overhead)| elements(material, labor, overhead)),
        children,
        truncated: false,
        issues: Vec::new(),
        warnings: Vec::new(),
    }
}

/// 外购料叶子（结构恒等于「全额材料」）。
fn purchased(level: u32, code: &str, amount: &str) -> NodeView {
    node(level, code, amount, Some((amount, "0", "0")), false, Vec::new())
}

fn before(material: &str, labor: &str, overhead: &str) -> ElementSplit {
    ElementSplit {
        material: dec(material),
        labor: dec(labor),
        overhead: dec(overhead),
    }
}

/// 三层链（与真库夹具同一组数）：成品 F 的材料 500 = 半成品 S 400 + 外购料 R 100。
///
/// ```text
/// R(外购)
///   └─ S2 批：R 500，完工成本结构 500/100/400
///        └─ S  批：S2 400 + R 100，完工成本结构 500/200/300
///             └─ F   批：S 400 + R 100（本期材料投入 500）
/// ```
fn three_level_tree() -> Vec<NodeView> {
    let leaf_in_s2 = purchased(3, "R", "500");
    let s2 = node(2, "S2", "400", Some(("200", "40", "160")), true, vec![leaf_in_s2]);
    let leaf_in_s = purchased(2, "R2", "100");
    let semi = node(1, "S", "400", Some(("200", "80", "120")), true, vec![s2, leaf_in_s]);
    let leaf_in_f = purchased(1, "R3", "100");
    vec![semi, leaf_in_f]
}

// ==================== 一、卷积 ====================

#[test]
fn purchased_leaf_is_unchanged() {
    // 材料全是外购料：还原前后一模一样（不画蛇添足）
    let tree = vec![purchased(1, "R", "250")];
    let rollup = dto::restore_of(&tree, before("250", "50", "50"));
    assert_eq!(
        rollup.after,
        ElementSplit {
            material: dec("250"),
            labor: dec("50"),
            overhead: dec("50"),
        },
        "还原后 = 还原前（材料不缩水、本期自有的工费照旧）"
    );
    assert_eq!(rollup.unresolved, Decimal::ZERO);
    assert_eq!(rollup.semis.len(), 0, "没有半成品");
    assert!(rollup.balanced());
    assert_eq!(dto::row_state(0, rollup.unresolved), RollupState::NoSemi);
}

#[test]
fn one_semi_level_moves_material_into_labor_and_overhead() {
    // S 600（结构 0.5 / 0.25 / 0.25）下面只有外购料 ⇒ 拆成 300 料 / 150 工 / 150 费
    let leaf = purchased(2, "R", "500");
    let semi = node(1, "S", "600", Some(("300", "150", "150")), true, vec![leaf]);
    let rollup = dto::restore_of(&[semi], before("600", "100", "50"));

    assert_eq!(rollup.after.material, dec("300"), "半成品的材料部分退回外购料");
    assert_eq!(rollup.after.labor, dec("250"), "150（还原）+ 100（本期自有人工）");
    assert_eq!(rollup.after.overhead, dec("200"), "150（还原）+ 50（本期自有制造费用）");
    assert_eq!(rollup.unresolved, Decimal::ZERO);
    assert!(rollup.balanced(), "还原后合计必须等于还原前合计");
    assert_eq!(rollup.semis.len(), 1);
    assert!(!rollup.semis[0].stopped, "钻到底了");

    let flows = dto::flow_views(&rollup.flows);
    assert_eq!(flows.len(), 2);
    assert_eq!(dto::dec(&flows[0].imputed), dec("600"), "第 1 层 = 本期材料投入");
    assert_eq!(dto::dec(&flows[0].purchased), dec("0"), "第 1 层没有外购料叶子");
    assert_eq!(dto::dec(&flows[0].carried_down), dec("300"), "半成品的材料部分继续往下");
    assert_eq!(dto::dec(&flows[1].imputed), dec("300"), "第 2 层是上一层的材料部分");
    assert_eq!(dto::dec(&flows[1].purchased), dec("300"));
    assert!(rollup.flows.iter().all(|flow| flow.balanced()));
}

#[test]
fn three_levels_are_convolved_recursively() {
    // 见 three_level_tree 的图：还原后 = 外购料 220 / 人工 196 / 制造费用 284
    let rollup = dto::restore_of(&three_level_tree(), before("500", "100", "100"));

    assert_eq!(
        rollup.after.material,
        dec("220"),
        "100（一级 R）+ 40（二级 R）+ 80（三级 R）"
    );
    assert_eq!(rollup.after.labor, dec("196"), "80 + 16 + 本期 100");
    assert_eq!(rollup.after.overhead, dec("284"), "120 + 64 + 本期 100");
    assert_eq!(rollup.after.total() + rollup.unresolved, dec("700"));
    assert!(rollup.balanced());

    // 逐层台账：每层「流入 = 外购 + 人工 + 费 + 继续下钻 + 不彻底」
    assert_eq!(rollup.flows.len(), 3, "三层");
    let l1 = &rollup.flows[0];
    assert_eq!(
        (l1.imputed, l1.purchased, l1.labor, l1.overhead),
        (dec("500"), dec("100"), dec("80"), dec("120"))
    );
    assert_eq!((l1.carried_down, l1.unresolved), (dec("200"), Decimal::ZERO));
    let l2 = &rollup.flows[1];
    assert_eq!(
        (l2.imputed, l2.purchased, l2.labor, l2.overhead),
        (dec("200"), dec("40"), dec("16"), dec("64"))
    );
    assert_eq!((l2.carried_down, l2.unresolved), (dec("80"), Decimal::ZERO));
    let l3 = &rollup.flows[2];
    assert_eq!(
        (l3.imputed, l3.purchased, l3.labor, l3.overhead),
        (dec("80"), dec("80"), Decimal::ZERO, Decimal::ZERO)
    );
    assert_eq!(l3.carried_down, Decimal::ZERO);
    assert!(rollup.flows.iter().all(|flow| flow.balanced()));

    // 半成品明细：S（第 1 层）+ S2（第 2 层）
    assert_eq!(rollup.semis.len(), 2);
    assert_eq!(rollup.semis[0].level, 1);
    assert_eq!(rollup.semis[0].restored.material, dec("120"));
    assert_eq!(rollup.semis[1].level, 2);
    assert_eq!(rollup.semis[1].restored.material, dec("80"));
}

#[test]
fn each_layer_of_the_ledger_sums_to_the_confirmed_totals() {
    // 台账各层确认的金额加起来 = 还原后的三要素（外购料 / 人工 / 费各一项）
    let rollup = dto::restore_of(&three_level_tree(), before("500", "100", "100"));
    let purchased: Decimal = rollup.flows.iter().map(|flow| flow.purchased).sum();
    let labor: Decimal = rollup.flows.iter().map(|flow| flow.labor).sum();
    let overhead: Decimal = rollup.flows.iter().map(|flow| flow.overhead).sum();
    let unresolved: Decimal = rollup.flows.iter().map(|flow| flow.unresolved).sum();
    assert_eq!(purchased, rollup.after.material);
    assert_eq!(labor, rollup.after.labor - dec("100"), "台账只含还原出来的部分");
    assert_eq!(overhead, rollup.after.overhead - dec("100"));
    assert_eq!(unresolved, rollup.unresolved);
}

#[test]
fn semi_without_structure_goes_to_unresolved_not_to_material() {
    // 缺结构：整笔钱停在半成品形态（**不假装成外购料**，也不丢掉）
    let semi = node(1, "S", "400", None, true, Vec::new());
    let rollup = dto::restore_of(&[semi], before("400", "0", "0"));
    assert_eq!(rollup.after.material, Decimal::ZERO);
    assert_eq!(rollup.unresolved, dec("400"));
    assert!(rollup.missing);
    assert!(rollup.balanced());
    assert_eq!(dto::row_state(1, rollup.unresolved), RollupState::Partial);
    assert!(rollup.semis[0].stopped);
    assert_eq!(rollup.semis[0].restored.unresolved, dec("400"));
}

#[test]
fn semi_without_children_stops_but_keeps_its_labor_and_overhead() {
    // 同月平均结构 / 没有下一层投入：工费照样拆出来，材料部分停在半成品形态
    let semi = node(1, "S", "400", Some(("200", "80", "120")), true, Vec::new());
    let rollup = dto::restore_of(&[semi], before("400", "0", "0"));
    assert_eq!(rollup.after.material, Decimal::ZERO);
    assert_eq!(rollup.after.labor, dec("80"));
    assert_eq!(rollup.after.overhead, dec("120"));
    assert_eq!(rollup.unresolved, dec("200"), "材料部分没得钻");
    assert!(rollup.balanced());
    assert!(rollup.semis[0].stopped);
    assert_eq!(rollup.flows[0].carried_down, Decimal::ZERO);
}

#[test]
fn truncated_semi_is_not_drilled_even_with_children() {
    // 闭环 / 层数上限：M5-26 已经在节点上截断了，这里不许再往下（否则第二本账）
    let leaf = purchased(2, "R", "500");
    let mut semi = node(1, "S", "400", Some(("200", "80", "120")), true, vec![leaf]);
    semi.truncated = true;
    let rollup = dto::restore_of(&[semi], before("400", "0", "0"));
    assert_eq!(rollup.after.material, Decimal::ZERO, "截断处不当外购料");
    assert_eq!(rollup.unresolved, dec("200"));
    assert!(rollup.truncated);
    assert_eq!(rollup.flows.len(), 1, "不往下展开");
    assert!(rollup.balanced());
}

#[test]
fn rounding_tail_lands_in_overhead() {
    // 1/3 除不尽：材料 33.3333 + 人工 33.3333 + 制造费用 33.3334 = 100
    let semi = node(1, "S", "100", Some(("1", "1", "1")), true, Vec::new());
    let rollup = dto::restore_of(&[semi], before("100", "0", "0"));
    assert_eq!(rollup.after.labor, dec("33.3333"));
    assert_eq!(rollup.after.overhead, dec("33.3334"), "尾差进制造费用");
    assert_eq!(rollup.unresolved, dec("33.3333"));
    assert!(rollup.balanced());
}

#[test]
fn sibling_shares_are_allocated_by_amount_and_conserve() {
    // 两个半成品兄弟：材料的份额按「兄弟金额占比」分，最后一档吃尾差
    let left = node(
        1,
        "SL",
        "300",
        Some(("100", "100", "100")),
        true,
        vec![purchased(2, "RL", "10")],
    );
    let right = node(
        1,
        "SR",
        "100",
        Some(("100", "0", "0")),
        true,
        vec![purchased(2, "RR", "10")],
    );
    let rollup = dto::restore_of(&[left, right], before("400", "0", "0"));
    // 左：材料 100 全给 RL ⇒ 外购 100；右：材料 100 全给 RR ⇒ 外购 100
    assert_eq!(rollup.after.material, dec("200"));
    assert_eq!(rollup.after.labor, dec("100"));
    assert_eq!(rollup.after.overhead, dec("100"));
    assert!(rollup.balanced());
    assert!(rollup.flows.iter().all(|flow| flow.balanced()));
}

#[test]
fn gap_between_tree_and_material_total_is_recorded_as_unresolved() {
    // 表被改过（第 1 层合计 ≠ 材料投入）：差额进「不彻底」，不让守恒等式失真
    let tree = vec![purchased(1, "R", "400")];
    let rollup = dto::restore_of(&tree, before("500", "0", "0"));
    assert_eq!(rollup.after.material, dec("400"));
    assert_eq!(rollup.unresolved, dec("100"));
    assert!(rollup.missing);
    assert!(rollup.balanced(), "还原后 + 不彻底 = 还原前");
    assert_eq!(rollup.covered, dec("400"));
}

#[test]
fn empty_tree_keeps_the_before_total() {
    // 没有材料（结构树为空）：还原后只剩本期自有的工费，总额不动
    let rollup = dto::restore_of(&[], before("0", "30", "20"));
    assert_eq!(rollup.after.material, Decimal::ZERO);
    assert_eq!(rollup.after.labor, dec("30"));
    assert_eq!(rollup.after.overhead, dec("20"));
    assert_eq!(rollup.unresolved, Decimal::ZERO);
    assert!(rollup.balanced());
    assert_eq!(rollup.flows.len(), 0);
}

#[test]
fn deep_chain_stays_conserved_at_every_node() {
    // 6 层链（每层把材料 8 折下去）：总额必须一分不差，且每层台账自洽
    let mut tree = vec![purchased(6, "R", "100")];
    for level in (1..6).rev() {
        tree = vec![node(
            level,
            &format!("S{level}"),
            "100",
            Some(("800", "100", "100")),
            true,
            tree,
        )];
    }
    let rollup = dto::restore_of(&tree, before("100", "0", "0"));
    assert_eq!(rollup.after.total() + rollup.unresolved, dec("100"));
    assert_eq!(rollup.flows.len(), 6);
    assert!(rollup.flows.iter().all(|flow| flow.balanced()));
}

// ==================== 二、状态与筛选 ====================

#[test]
fn row_state_prefers_partial_over_restored() {
    assert_eq!(
        dto::row_state(0, dec("10")),
        RollupState::NoSemi,
        "没有半成品 ⇒ 无需还原"
    );
    assert_eq!(dto::row_state(1, Decimal::ZERO), RollupState::Restored);
    assert_eq!(dto::row_state(1, dec("0.0001")), RollupState::Partial);
}

#[test]
fn state_filter_parses_and_matches() {
    assert_eq!(StateFilter::parse(None).expect("默认"), StateFilter::All);
    assert_eq!(StateFilter::parse(Some("  ")).expect("空白按默认"), StateFilter::All);
    assert_eq!(
        StateFilter::parse(Some("restored")).expect("已还原"),
        StateFilter::Restored
    );
    assert_eq!(
        StateFilter::parse(Some("partial")).expect("不彻底"),
        StateFilter::Partial
    );
    assert_eq!(
        StateFilter::parse(Some("no_semi")).expect("无需还原"),
        StateFilter::NoSemi
    );
    let error = StateFilter::parse(Some("done")).expect_err("拼错必须 400");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("done"));

    assert!(StateFilter::All.matches(RollupState::Partial));
    assert!(StateFilter::Restored.matches(RollupState::Restored));
    assert!(!StateFilter::Restored.matches(RollupState::Partial));
    assert!(StateFilter::Partial.matches(RollupState::Partial));
    assert!(StateFilter::NoSemi.matches(RollupState::NoSemi));

    let options = dto::state_options();
    assert_eq!(options.len(), 4);
    assert_eq!(options[0].value, "all");
    assert_eq!(options[3].value, "no_semi");
}

// ==================== 三、出参装配 ====================

#[test]
fn delta_view_never_moves_the_total() {
    let before = before("500", "100", "100");
    let after = ElementSplit {
        material: dec("220"),
        labor: dec("196"),
        overhead: dec("284"),
    };
    let view = dto::delta_view(before, after);
    assert_eq!(dto::dec(&view.material), dec("-280"));
    assert_eq!(dto::dec(&view.labor), dec("96"));
    assert_eq!(dto::dec(&view.overhead), dec("184"));
    assert_eq!(dto::dec(&view.total), Decimal::ZERO, "还原不动总额");
}

#[test]
fn parse_elements_rejects_empty_structure() {
    let zero = elements("0", "0", "0");
    assert!(dto::parse_elements(&zero).is_none(), "全 0 结构当作取不到");
    let normal = elements("1", "2", "3");
    assert_eq!(
        dto::parse_elements(&normal),
        Some(ElementSplit {
            material: Decimal::ONE,
            labor: Decimal::from(2),
            overhead: Decimal::from(3),
        })
    );
}

#[test]
fn flow_views_explain_each_level() {
    let rollup = dto::restore_of(&three_level_tree(), before("500", "100", "100"));
    let views = dto::flow_views(&rollup.flows);
    assert!(views[0].note.contains("第 1 层"));
    assert!(views[1].note.contains("不是新增投入"), "第 2 层起要说明不是加总关系");
}

#[test]
fn semi_views_carry_restored_elements() {
    let rollup = dto::restore_of(&three_level_tree(), before("500", "100", "100"));
    let views = dto::semi_views(&rollup.semis);
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].material_code.as_deref(), Some("S"));
    assert_eq!(dto::dec(&views[0].imputed), dec("400"));
    assert_eq!(dto::dec(&views[0].elements.material), dec("120"));
    assert_eq!(dto::dec(&views[0].elements.labor), dec("96"));
    assert_eq!(dto::dec(&views[0].elements.overhead), dec("184"));
    assert_eq!(dto::dec(&views[0].unresolved), Decimal::ZERO);
    assert!(!views[0].stopped);
    assert_eq!(views[1].material_code.as_deref(), Some("S2"));
    assert_eq!(dto::dec(&views[1].imputed), dec("160"), "被卷积的是父层拨下来的份额");
}

#[test]
fn totals_view_check_is_zero_and_shares_are_pct() {
    let totals = dto::Totals {
        before_material: dec("500"),
        before_labor: dec("100"),
        before_overhead: dec("100"),
        before_total: dec("700"),
        after_material: dec("220"),
        after_labor: dec("196"),
        after_overhead: dec("284"),
        after_total: dec("700"),
        ..dto::Totals::default()
    };
    let view = dto::totals_view(&totals);
    assert_eq!(dto::dec(&view.check), Decimal::ZERO);
    assert_eq!(view.after_material_share.as_deref(), Some("31.43"));
    assert_eq!(view.after_labor_share.as_deref(), Some("28.00"));
    assert_eq!(view.after_overhead_share.as_deref(), Some("40.57"));
    assert!(view.note.contains("不是加总关系"));
}

#[test]
fn formula_texts_state_the_invariants() {
    assert!(dto::FORMULA_RESTORE.contains("尾差进制造费用"));
    assert!(dto::FORMULA_UNRESOLVED.contains("还原后合计 + 还原不彻底 = 还原前合计"));
}

#[test]
fn permission_code_is_the_shared_one() {
    assert_eq!(dto::PERM_LIST, "business:erp:cost-object:list");
    assert_eq!(dto::PERMISSIONS.len(), 1);
}
