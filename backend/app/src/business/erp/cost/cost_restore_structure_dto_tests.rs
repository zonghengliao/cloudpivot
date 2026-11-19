//! 成本还原「半成品成本结构表」的纯逻辑单测（**不连库**：口径、比例、来源解析、状态取档、逐层汇总）。
//!
//! 真库用例在 `cost_restore_structure_db_tests`（走 `ERP_DB_URL`）。
//! 这里钉住的是「算得对不对、取哪一档、null 还是 0」——库里那部分只验证取数形状。

use crate::business::erp::cost::cost_restore_structure_dto::*;
use nik::prelude::rust_decimal::Decimal;
use std::str::FromStr;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字面量：{text}"))
}

fn item(completed: &str, finished: &str, unit_mat: &str, unit_labor: &str, unit_oh: &str) -> ItemFacts {
    ItemFacts {
        status: "SETTLED".to_string(),
        completed_qty: dec(completed),
        finished_cost: dec(finished),
        unit_finished_cost: if dec(completed).is_zero() {
            Decimal::ZERO
        } else {
            dec(finished) / dec(completed)
        },
        unit_material: dec(unit_mat),
        unit_labor: dec(unit_labor),
        unit_overhead: dec(unit_oh),
    }
}

fn node(level: u32, amount: &str, is_semi: bool, elements: Option<ElementView>) -> NodeView {
    NodeView {
        level,
        material_id: format!("mat-{level}"),
        material_code: Some(format!("M-{level}")),
        material_name: Some(format!("物料{level}")),
        lot_no: None,
        qty: "1.0000".to_string(),
        unit_cost: None,
        amount: amount.to_string(),
        share_pct: None,
        is_semi,
        source: None,
        source_label: None,
        producer: None,
        elements,
        children: Vec::new(),
        truncated: false,
        issues: Vec::new(),
        warnings: Vec::new(),
    }
}

fn split(material: &str, labor: &str, overhead: &str) -> ElementSplit {
    ElementSplit {
        material: dec(material),
        labor: dec(labor),
        overhead: dec(overhead),
    }
}

// ==================== 完工成本结构 ====================

#[test]
fn structure_splits_finished_cost_by_unit_costs() {
    // 完工 100 × (料 2 + 工 1 + 费 0.5) = 350
    let facts = item("100", "350", "2", "1", "0.5");
    let structure = structure_of(&facts).expect("有结构");
    assert_eq!(structure.material, dec("200"));
    assert_eq!(structure.labor, dec("100"));
    assert_eq!(structure.overhead, dec("50"));
    assert_eq!(structure.total(), dec("350"), "三要素合计 = 完工转出");
}

#[test]
fn structure_puts_the_rounding_tail_into_overhead() {
    // 单位成本是 6 位、金额是 4 位：尾差 0.0001 必须落进制造费用，不能让三要素加不齐
    let facts = item("100", "350.0001", "2", "1", "0.5");
    let structure = structure_of(&facts).expect("有结构");
    assert_eq!(structure.material, dec("200"));
    assert_eq!(structure.labor, dec("100"));
    assert_eq!(structure.overhead, dec("50.0001"), "尾差进制造费用");
    assert_eq!(structure.total(), dec("350.0001"));
}

#[test]
fn structure_of_zero_output_is_none_not_zero() {
    // 没完工 / 完工成本 0：结构无从谈起，给 None（不拿 0 冒充「全是材料」）
    assert!(structure_of(&item("0", "0", "2", "1", "0.5")).is_none());
    assert!(structure_of(&item("10", "0", "2", "1", "0.5")).is_none());
}

#[test]
fn structure_of_negative_finished_cost_is_none() {
    assert!(structure_of(&item("10", "-5", "0", "0", "0")).is_none());
}

#[test]
fn structure_never_returns_negative_elements() {
    // 病态数据：完工成本比「材料 + 人工」还小 —— 尾差从人工里拨，仍凑不出 ⇒ None（不给负数）
    let mut facts = item("10", "15", "10", "10", "0");
    facts.finished_cost = dec("15");
    assert!(structure_of(&facts).is_none_or(|split| split.material >= Decimal::ZERO
        && split.labor >= Decimal::ZERO
        && split.overhead >= Decimal::ZERO));
}

// ==================== 比例拆分 ====================

#[test]
fn scale_split_keeps_the_node_amount_exactly() {
    let structure = split("60", "20", "20");
    let out = scale_split(dec("100"), &structure).expect("可拆");
    assert_eq!(out.material, dec("60"));
    assert_eq!(out.labor, dec("20"));
    assert_eq!(out.overhead, dec("20"));
    assert_eq!(out.total(), dec("100"), "拆完合计恒等于节点金额");
}

#[test]
fn scale_split_puts_the_tail_into_overhead() {
    let structure = split("1", "1", "1");
    let out = scale_split(dec("10"), &structure).expect("可拆");
    assert_eq!(out.material, dec("3.3333"));
    assert_eq!(out.labor, dec("3.3333"));
    assert_eq!(out.overhead, dec("3.3334"), "10 − 6.6666 = 3.3334（尾差进费用）");
    assert_eq!(out.total(), dec("10"));
}

#[test]
fn scale_split_of_zero_structure_is_none() {
    // 结构合计 0 时没有比例可依：给 None，而不是「全额进材料」
    assert!(scale_split(dec("100"), &ElementSplit::ZERO).is_none());
    assert!(scale_split(dec("-1"), &split("1", "0", "0")).is_none());
}

#[test]
fn material_only_leaf_puts_everything_into_material() {
    let leaf = ElementSplit::material_only(dec("40"));
    assert_eq!(leaf.material, dec("40"));
    assert_eq!(leaf.labor, Decimal::ZERO);
    assert_eq!(leaf.overhead, Decimal::ZERO);
    assert_eq!(leaf.total(), dec("40"));
    assert!(!leaf.is_zero());
}

#[test]
fn share_pct_is_two_digits_and_none_for_zero_base() {
    assert_eq!(share_pct(dec("60"), dec("100")), Some(dec("60.00")));
    assert_eq!(share_pct(dec("1"), dec("3")), Some(dec("33.33")));
    assert_eq!(share_pct(dec("1"), Decimal::ZERO), None, "分母 0 不画 0%");
}

// ==================== 生产者解析 ====================

fn producer(
    lot: Option<&str>,
    period: &str,
    status: &str,
    completed: &str,
    finished: &str,
    units: (&str, &str, &str),
) -> ProducerFacts {
    ProducerFacts {
        product_id: "mat-semi".to_string(),
        lot_no: lot.map(str::to_string),
        object_id: format!("obj-{lot:?}-{period}"),
        object_no: format!("CB-{lot:?}-{period}"),
        period_code: period.to_string(),
        item: ItemFacts {
            status: status.to_string(),
            completed_qty: dec(completed),
            finished_cost: dec(finished),
            unit_finished_cost: if dec(completed).is_zero() {
                Decimal::ZERO
            } else {
                dec(finished) / dec(completed)
            },
            unit_material: dec(units.0),
            unit_labor: dec(units.1),
            unit_overhead: dec(units.2),
        },
    }
}

#[test]
fn producer_batch_hit_wins_over_period_average() {
    // 两个批次：10 月的 A 批（料 60/工 20/费 20），11 月的 B 批（料 100/工 0/费 0）
    let producers = vec![
        producer(Some("LOT-A"), "2026-10", "SETTLED", "100", "500", ("3", "1", "1")),
        producer(Some("LOT-B"), "2026-11", "SETTLED", "100", "200", ("2", "0", "0")),
    ];
    let choice = resolve_producer(&producers, Some("LOT-A")).expect("批号对得上");
    assert_eq!(choice.source, ProduceSource::Batch);
    assert_eq!(choice.period_code, "2026-10");
    assert_eq!(choice.batch_count, 1);
    assert_eq!(choice.finished_cost, dec("500"));
    assert_eq!(choice.structure.material, dec("300"), "100 件 × 单位材料 3");
    assert_eq!(choice.structure.total(), dec("500"));
}

#[test]
fn producer_batch_miss_falls_back_to_the_latest_period_average() {
    let producers = vec![
        producer(Some("LOT-A"), "2026-10", "SETTLED", "100", "500", ("3", "1", "1")),
        producer(Some("LOT-B"), "2026-11", "SETTLED", "100", "200", ("2", "0", "0")),
    ];
    let choice = resolve_producer(&producers, Some("LOT-Z")).expect("退到同月平均");
    assert_eq!(choice.source, ProduceSource::PeriodAvg, "批号对不上不能假装按批");
    assert_eq!(choice.period_code, "2026-11", "只取最近一个期间");
    assert_eq!(choice.batch_count, 1, "11 月只有 B 一批");
    assert_eq!(choice.structure.material, dec("200"));
    assert_eq!(choice.lot_no.as_deref(), Some("LOT-B"), "只有一批能报批号");
}

#[test]
fn producer_without_lot_uses_period_average_of_that_period_only() {
    let producers = vec![
        producer(Some("LOT-A"), "2026-10", "SETTLED", "100", "500", ("3", "1", "1")),
        producer(Some("LOT-B"), "2026-11", "SETTLED", "100", "200", ("2", "0", "0")),
        producer(Some("LOT-C"), "2026-11", "CALC", "300", "600", ("1", "0", "1")),
    ];
    let choice = resolve_producer(&producers, None).expect("按同月平均");
    assert_eq!(choice.period_code, "2026-11");
    assert_eq!(choice.batch_count, 2, "同月两批都参与");
    assert_eq!(choice.completed_qty, dec("400"));
    assert_eq!(choice.finished_cost, dec("800"));
    // 料 = 100×2 + 300×1 = 500；工 = 0；费 = 200 + 300×1 = 500
    assert_eq!(choice.structure.material, dec("500"));
    assert_eq!(choice.structure.labor, dec("0"));
    assert_eq!(choice.structure.overhead, dec("300"));
    assert_eq!(choice.structure.total(), dec("800"));
    assert!(choice.lot_no.is_none(), "多批不一致时不报批号");
    assert_eq!(choice.item_status, "SETTLED", "取有 SETTLED 的那批状态");
}

#[test]
fn producer_prefers_settled_when_lot_matches_twice_in_one_period() {
    // 同一批号跨期重算：取最近一期；同期间还有一条 CALC 时以 SETTLED 为准
    let producers = vec![
        producer(Some("LOT-A"), "2026-11", "CALC", "100", "400", ("2", "1", "1")),
        producer(Some("LOT-A"), "2026-11", "SETTLED", "100", "500", ("3", "1", "1")),
        producer(Some("LOT-A"), "2026-10", "SETTLED", "10", "10", ("1", "0", "0")),
    ];
    let choice = resolve_producer(&producers, Some("LOT-A")).expect("命中");
    assert_eq!(choice.period_code, "2026-11", "最近一期");
    assert_eq!(choice.item_status, "SETTLED", "同期优先已结算");
    assert_eq!(choice.finished_cost, dec("500"));
}

#[test]
fn producer_without_any_structure_is_none() {
    // 有产出批次但完工成本 0（本期没产出 / 计算单为空）⇒ 结构取不到
    let producers = vec![producer(Some("LOT-A"), "2026-11", "CALC", "0", "0", ("0", "0", "0"))];
    assert!(resolve_producer(&producers, Some("LOT-A")).is_none());
    assert!(resolve_producer(&producers, None).is_none());
}

#[test]
fn producer_empty_list_is_none() {
    // 没有任何成本对象产出过 ⇒ 外购材料（叶子），不是「缺结构」
    assert!(resolve_producer(&[], Some("LOT-A")).is_none());
    assert!(resolve_producer(&[], None).is_none());
}

// ==================== 行状态 ====================

#[test]
fn row_state_precedence_is_cycle_first() {
    assert_eq!(row_state(dec("10"), true, true, true), RowState::Cycle);
    assert_eq!(row_state(dec("10"), true, false, true), RowState::NoStructure);
    assert_eq!(row_state(dec("10"), false, false, true), RowState::Truncated);
    assert_eq!(row_state(dec("10"), false, false, false), RowState::Semi);
    assert_eq!(row_state(Decimal::ZERO, false, false, false), RowState::NoSemi);
}

#[test]
fn row_state_labels_are_short() {
    for state in [
        RowState::Semi,
        RowState::NoSemi,
        RowState::NoStructure,
        RowState::Cycle,
        RowState::Truncated,
    ] {
        assert!(state.label().chars().count() <= 6, "列标要短：{}", state.label());
        assert!(!state.as_str().is_empty());
    }
}

#[test]
fn state_filter_parses_and_matches() {
    assert_eq!(StateFilter::parse(None).expect("默认 all"), StateFilter::All);
    assert_eq!(StateFilter::parse(Some(" semi ")).expect("trim"), StateFilter::Semi);
    assert_eq!(
        StateFilter::parse(Some("no_semi")).expect("下划线"),
        StateFilter::NoSemi
    );
    let error = StateFilter::parse(Some("semis")).expect_err("拼错必须 400");
    assert_eq!(error.status, 400);

    assert!(StateFilter::Blocked.matches(RowState::NoStructure));
    assert!(StateFilter::Blocked.matches(RowState::Cycle));
    assert!(StateFilter::Blocked.matches(RowState::Truncated));
    assert!(!StateFilter::Blocked.matches(RowState::Semi));
    assert!(StateFilter::Semi.matches(RowState::Semi));
    assert!(!StateFilter::Semi.matches(RowState::NoSemi));
    assert_eq!(state_options().len(), 4);
}

// ==================== 逐层汇总 ====================

#[test]
fn layer_views_aggregate_pure_levels() {
    // 第 1 层：半成品 60（结构 36/12/12）+ 外购 40；第 2 层：外购 20（有结构）+ 20（无结构）
    let mut semi = node(1, "60", true, Some(split("36", "12", "12").view()));
    let mut purchased = node(1, "40", false, Some(ElementSplit::material_only(dec("40")).view()));
    purchased.children = vec![
        node(2, "20", false, Some(ElementSplit::material_only(dec("20")).view())),
        node(2, "20", true, None),
    ];
    semi.children = Vec::new();
    let layers = layer_views(&[semi, purchased]);
    assert_eq!(layers.len(), 2);

    assert_eq!(layers[0].level, 1);
    assert_eq!(layers[0].node_count, 2);
    assert_eq!(layers[0].semi_node_count, 1);
    assert_eq!(layers[0].amount, "100");
    assert_eq!(layers[0].elements.material, "76", "36 + 40");
    assert_eq!(layers[0].elements.labor, "12");
    assert_eq!(layers[0].elements.overhead, "12");
    assert_eq!(layers[0].incomplete_count, 0);
    assert!(layers[0].note.contains("等于材料投入"));

    assert_eq!(layers[1].node_count, 2);
    assert_eq!(layers[1].amount, "40");
    assert_eq!(layers[1].elements.material, "20", "只累加取得到结构的那个节点");
    assert_eq!(layers[1].incomplete_count, 1);
    assert!(layers[1].note.contains("不是父层金额的一部分"));
}

#[test]
fn layer_views_of_empty_tree_is_empty() {
    assert!(layer_views(&[]).is_empty());
    assert_eq!(count_nodes(&[]), 0);
    assert_eq!(max_level(&[]), 0);
}

#[test]
fn totals_add_up_rows_and_keep_null_finished_out() {
    let mut totals = Totals::default();
    let mut row = Row {
        object_id: "o1".to_string(),
        object_no: "CB-1".to_string(),
        object_type: "BATCH".to_string(),
        lot_no: None,
        product_id: "p1".to_string(),
        product_code: None,
        product_name: None,
        workshop_dept_id: "d1".to_string(),
        workshop_dept_name: None,
        item: None,
        finished_cost: Some("350".to_string()),
        material_cost: "100".to_string(),
        semi_cost: "60".to_string(),
        purchased_cost: "40".to_string(),
        semi_share: Some("60.00".to_string()),
        state: "semi".to_string(),
        state_label: "逐层可还原".to_string(),
        depth: 2,
        node_count: 3,
        layers: Vec::new(),
        tree: Vec::new(),
        issues: Vec::new(),
        warnings: Vec::new(),
    };
    totals.add(&row);
    row.finished_cost = None; // 没计算单的行：完工成本不参与合计
    row.state = "no_structure".to_string();
    totals.add(&row);

    let view = totals_view(&totals);
    assert_eq!(view.row_count, 2);
    assert_eq!(view.semi_row_count, 1);
    assert_eq!(view.no_structure_row_count, 1);
    assert_eq!(view.blocked_row_count, 1);
    assert_eq!(view.finished_cost, "350", "只有一行的完工成本进了合计");
    assert_eq!(view.finished_row_count, 1);
    assert_eq!(view.material_cost, "200");
    assert_eq!(view.semi_cost, "120");
    assert_eq!(view.semi_share.as_deref(), Some("60.00"));
    assert_eq!(view.max_depth, 2);
    assert_eq!(view.node_count, 6);
}

// ==================== 文本 ====================

#[test]
fn texts_keep_the_column_scale() {
    assert_eq!(money_text(dec("1000.0000")), "1000", "金额去尾随 0（成本域口径）");
    assert_eq!(money_text(dec("0.0001")), "0.0001");
    assert_eq!(qty_text(dec("1000")), "1000.0000", "数量固定 4 位，逐字可比");
    assert_eq!(pct_text(dec("33.333")), "33.33");
    assert_eq!(rate_text(dec("1.5")), "1.500000", "单价 6 位");
}

#[test]
fn element_view_totals_the_three_parts() {
    let view = split("1.0001", "2", "3").view();
    assert_eq!(view.material, "1.0001");
    assert_eq!(view.labor, "2");
    assert_eq!(view.overhead, "3");
    assert_eq!(view.total, "6.0001");
}

#[test]
fn dto_permission_set_is_the_shared_read_code() {
    assert_eq!(PERMISSIONS, [PERM_LIST]);
    assert_eq!(PERM_LIST, "business:erp:cost-object:list");
    assert!(FORMULA_LAYER.contains("自制半成品"));
    assert!(FORMULA_ELEMENT.contains("制造费用"));
}
