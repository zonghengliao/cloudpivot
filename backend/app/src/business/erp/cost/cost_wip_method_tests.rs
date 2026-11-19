//! 在制品「约当产量法」**全链路单测**（成本核算 · M5-24）—— **含多工序**。
//!
//! 本卡不新增任何业务能力：它把 M5-21（完工率按工序录入）/ M5-20（约当产量表）/
//! M5-22（料工费在完工与在制间分配）/ M5-23（期末在制余额 + 结转核对）四张卡
//! **串成一条链跑一遍**，钉住「多工序 → 约当 → 分配 → 余额」这条路上每一道接口。
//!
//! # 为什么要有这一张卡（四张卡各自都有单测了）
//!
//! M5-20/22/23 的 DTO 单测证明的是「每个函数按契约工作」，M5-21 的真库用例证明的是
//! 「工序表 ↔ 对象级完工率」在同一个事务里一起变。四者拼在一起时仍然有三个问题没人钉：
//!
//! 1. **多工序到底怎么进约当**：一期没有 MES 工序过站（ARCH-9 D-3），所以在制数量是
//!    **对象级一个数**，工序只贡献**加权综合完工率**（`rollup_of` 写回
//!    `biz_cost_object.completion_pct`）。换句话说多工序不是「按工序分别约当」，
//!    而是「按工序加权出一个完工率，再乘同一份在制数量」。本卡的第一条用例就把这件事
//!    钉死，避免后来人以为要按工序拆在制。
//! 2. **同一个数在三张表里是不是同一个数**：约当表给分母、分配表给分子、余额表给期末。
//!    各自都对、拼起来错（比如分配表用了未舍入约当而余额表用了显示值）是最难查的那类 bug。
//! 3. **一道工序的完工率改了，三张表要一起动、且都不许画假 0**。
//!
//! # 被钉住的口径（逐条对应用例）
//!
//! ```text
//! 综合完工率 P = Σ(工序权重 × 工序完工率) / Σ权重      （2 位小数，四舍五入取远）
//! 本期完工 FQ  = 累计完工 − 前期已结算计算单完工合计
//! 材料约当     = FQ + WQ × 100%                        （开工一次投入）
//! 加工约当     = FQ + WQ × P/100                       （P 是**回写后的 2 位值**）
//! 逐要素池     = 期初（上期单位成本还原）+ 本期投入
//! 分配率       = 池 ÷ 该要素约当（未舍入）              （6 位）
//! 完工分配     = FQ × 分配率（4 位）；在制 = 池 − 完工（4 位，差额倒挤）
//! 期末余额     = 期初 + 投入 − 完工转出 = Σ 三要素在制
//! 结转         = 已结算快照 vs 下期期初（carried / mismatch / …）
//! ```
//!
//! # 覆盖的验收项
//!
//! ## A. 跨卡链路（纯逻辑，不连库 —— 没配 `ERP_DB_URL` 也能绿）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`chain::w1_three_operations_roll_up_to_a_weighted_completion_pct`] | 三道工序 → 加权综合完工率（4 是权重合计、200 是加权和、50 是回写值）；逐行贡献之和 = 综合值 |
//! | 2 | [`chain::w2_weights_move_the_pct_and_the_equivalents`] | 同样的工序完工率、换权重 ⇒ P 从 50 变 70，加工约当 900→940，分配随之变（守恒不变） |
//! | 3 | [`chain::w3_weighted_pct_is_the_only_bridge_to_the_equivalence`] | 多工序只通过 `completion_pct` 影响约当：五档 P 逐档与 calc 的 `equivalents` 逐位相等，分配表的分母与约当表同源 |
//! | 4 | [`chain::w4_three_elements_split_and_conserve_under_the_multi_operation_rate`] | 三要素各自「池 ÷ 约当 × 完工数」+ 在制倒挤；材料吃材料约当、工/费同吃加工约当 |
//! | 5 | [`chain::w5_balance_rolls_the_allocation_without_a_second_formula`] | 余额表逐要素直接吃分配结果（不另起公式）：期初 + 投入 − 完工 = 期末，三要素加总 = 余额 |
//! | 6 | [`chain::w6_prior_unit_costs_become_this_period_element_beginnings`] | 跨月：上期已结算的单位成本还原成期初三要素（材料按 WQ、人工按约当、费用吃尾差） |
//! | 7 | [`chain::w7_a_settled_balance_carries_into_the_next_period`] | 已结算 ⇒ 余额取账面快照；下期期初一分不差 ⇒ `carried`，差一分 ⇒ `mismatch` 且给差额 |
//! | 8 | [`chain::w8_zero_progress_blocks_every_table_and_never_fakes_zero`] | 在制 > 0 而工序汇总 0%：三张表全部 `blocked`，金额一律 `null`（不是 0） |
//! | 9 | [`chain::w9_finished_batch_pushes_the_whole_pool_into_finished_goods`] | 期末在制 0：整池（含尾差）进完工，余额 0 ⇒ `cleared` |
//! | 10 | [`chain::w10_the_chain_uses_the_two_decimal_pct_the_operations_wrote_back`] | 链上用的是**回写后的 2 位 P**（22.23），不是未舍入商（22.226667）—— 尾差口径只能有一处 |
//! | 11 | [`chain::w11_operations_warnings_survive_the_multi_operation_case`] | 录入侧提醒（全 0 进度 / 在制为 0 / 权重全相同 / 对象级被改）在链路上仍成立，且不误报 |
//! | 12 | [`chain::w12_single_operation_weights_never_move_its_own_pct`] | 单工序：权重怎么填 P 都等于该工序完工率；两道工序起权重才起作用 |
//! | 13 | [`chain::w13_the_three_wip_tables_share_one_permission_code`] | 三张表同一个码 `business:erp:cost-object:list`（零新增码），工序表用的是 `list` / `update` |
//! | 14 | [`chain::w14_totals_of_two_rows_keep_the_three_element_identity`] | 两行合计：约当 / 分配 / 余额三张表的合计口径一致（余额合计 = 三要素合计，坏行不进金额） |
//!
//! ## B. 真库全链路（`ERP_DB_URL` 开关；不设则全部 SKIP）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 15 | [`db::d1_multi_operation_pct_drives_equivalence_allocation_and_balance`] | **主链路**：三道工序（1×100 + 2×50 + 1×0）/4 = 50% 回写对象级 → 约当 1000/900 → 三要素分配 2000/350 → 余额 2350 − 2000 = 350，三张表同数 |
//! | 16 | [`db::d2_prior_settled_period_feeds_the_beginnings_through_the_operations`] | 跨月：上期已结算（单位成本 5/3/2）→ 本期期初 500/150/100 → 池 3200 → 余额 700（真实 handler 取数，不是手抄） |
//! | 17 | [`db::d3_one_operation_change_moves_all_three_tables`] | 只把「焊接」从 50% 改成 100% ⇒ P 75 ⇒ 约当 950 ⇒ 分配与余额一起变，三张表仍然是同一个数 |
//! | 18 | [`db::d4_blocked_row_is_null_in_all_three_tables`] | 在制 200 / 完工率 0（无工序）：三张表都 `blocked`、金额 `null`、余额不进合计 |
//! | 19 | [`db::d5_totals_of_finished_and_wip_rows_keep_the_identity`] | 一行全完工 + 一行有在制：三张表合计同数（余额合计 = 料 + 工 + 费），完工行不进余额 |
//! | 20 | [`db::zz_teardown_leaves_zero_residue`] | 清场后本卡造的行全 0（对象 / 工序 / 归集 / 计算单 / 批次） |
//!
//! # 夹具纪律
//!
//! * 纯逻辑用例**只用 DTO 的纯函数**（`specs_of` / `rollup_of` / `equiv_of` / `alloc_of` /
//!   `balance_of` / `carry_of`），不连库、不读时钟；
//! * 真库用例走**真实 handler**（`save_operations` / `equiv_table_of` / `alloc_table_of` /
//!   `balance_table_of`，只跳过认证），成本对象与归集行用 SQL 造（接口造不出来）；
//! * 每个夹具独占一个期间槽（`2000 + slot % 70` 年的 01/02/03），建前先清该槽残留；
//! * 对象号 / 工序名 / 批号全部带唯一后缀，列表断言一律用 `keyword` 收窄到本夹具；
//! * 金额断言用 `Decimal` 解析后比较，避免 `1000` vs `1000.0000` 的标度假红。

use nik::prelude::*;

// ==================== A. 跨卡链路（纯逻辑） ====================

#[cfg(test)]
mod chain {
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    use crate::business::erp::cost::cost_period_item_dto as item_dto;
    use crate::business::erp::cost::cost_wip_alloc_dto::{self as alloc, AllocFacts, Element, PriorFacts, StoredItem};
    use crate::business::erp::cost::cost_wip_balance_dto::{
        self as balance, BatchRef, BookBalance, CarryState, NextFacts,
    };
    use crate::business::erp::cost::cost_wip_equiv_dto::{self as equiv, EquivFacts, EquivState};
    use crate::business::erp::cost::cost_wip_pct_dto::{
        self as pct_dto, OperationRowReq, OperationSaveReq, OperationSpec, Rollup,
    };

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数量字面量无法解析：{text}"))
    }

    // ---------- 工序 → 综合完工率 ----------

    fn row(seq: i32, name: &str, weight: Option<&str>, pct: &str) -> OperationRowReq {
        OperationRowReq {
            seq: Some(seq),
            operation_name: Some(name.to_string()),
            weight: weight.map(dec),
            completion_pct: Some(dec(pct)),
            remark: None,
        }
    }

    /// 入参 → 归一化规格 + 加权汇总（走 `specs_of` / `rollup_of`，与接口同一条路径）。
    fn rollup(rows: Vec<OperationRowReq>) -> (Vec<OperationSpec>, Rollup) {
        let specs = pct_dto::specs_of(&OperationSaveReq { rows }).expect("工序入参应合法");
        let rollup = pct_dto::rollup_of(&specs).expect("非空工序表必须有汇总");
        (specs, rollup)
    }

    /// 三道工序的标准夹具：下料 100%（权重 1）/ 焊接 50%（权重 2）/ 装配 0%（权重 1）。
    /// ⇒ (1×100 + 2×50 + 1×0) / 4 = 50%。
    fn three_ops() -> (Vec<OperationSpec>, Rollup) {
        rollup(vec![
            row(1, "下料", Some("1"), "100"),
            row(2, "焊接", Some("2"), "50"),
            row(3, "装配", Some("1"), "0"),
        ])
    }

    // ---------- 事实构造 ----------

    /// 约当事实（本期完工 800 / 在制 200 / 综合完工率由工序给出）。
    fn equiv_facts(pct: Decimal, completed: &str, wip: &str, prior_completed: &str) -> EquivFacts {
        EquivFacts {
            object_no: "CB-M524-001".to_string(),
            material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec(completed),
            prior_completed_qty: dec(prior_completed),
            wip_qty: dec(wip),
            completion_pct: pct,
            collected_total: dec("2350"),
            prior_wip_end_qty: Decimal::ZERO,
            stored: None,
        }
    }

    /// 分配事实：本期投入 料 1000 / 工 900 / 费 450（总额 2350）。
    fn alloc_facts(pct: Decimal, completed: &str, wip: &str, prior_completed: &str) -> AllocFacts {
        AllocFacts {
            object_no: "CB-M524-001".to_string(),
            material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec(completed),
            prior_completed_qty: dec(prior_completed),
            wip_qty: dec(wip),
            completion_pct: pct,
            mat_cur: dec("1000"),
            labor_cur: dec("900"),
            oh_cur: dec("450"),
            prior: None,
            stored: None,
        }
    }

    /// 上期已结算（单位材料 5 / 单位人工 3 / 单位制造费用 2，期末在制 100 @ 50%，
    /// 期末在制成本 750 ⇒ 材料 500 / 人工 150 / 费用 100）。
    fn prior_facts() -> PriorFacts {
        PriorFacts {
            period_code: "2026-01".to_string(),
            wip_end_qty: dec("100"),
            wip_end_cost: dec("750"),
            completion_pct: dec("50"),
            unit_material: dec("5"),
            unit_labor: dec("3"),
            unit_overhead: dec("2"),
        }
    }

    /// 跨月的分配事实：累计完工 800、前期已结算完工 300 ⇒ 本期完工 500；
    /// 在制 200 @ 50%；本期投入 料 900 / 工 1050 / 费 500（总额 2450）。
    fn cross_month_facts() -> AllocFacts {
        AllocFacts {
            object_no: "CB-M524-002".to_string(),
            material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec("800"),
            prior_completed_qty: dec("300"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            mat_cur: dec("900"),
            labor_cur: dec("1050"),
            oh_cur: dec("500"),
            prior: Some(prior_facts()),
            stored: None,
        }
    }

    /// 与 `cross_month_facts` 完全一致的本期计算单快照（已结算）。
    fn settled_item() -> StoredItem {
        StoredItem {
            status: "SETTLED".to_string(),
            wip_beg_cost: dec("750"),
            completed_qty: dec("500"),
            wip_end_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("700"),
            equiv_conversion: dec("600"),
            unit_material: dec("2"),
            unit_labor: dec("2"),
            unit_overhead: dec("1"),
            finished_cost: dec("2500"),
            wip_end_cost: dec("700"),
        }
    }

    fn element_of(calc: &alloc::AllocCalc, element: Element) -> &alloc::ElementAlloc {
        calc.element_of(element)
    }

    // ==================== 1. 多工序 → 综合完工率 ====================

    #[test]
    fn w1_three_operations_roll_up_to_a_weighted_completion_pct() {
        let (specs, rollup) = three_ops();

        assert_eq!(specs.len(), 3, "三道工序都归一化进来了");
        assert_eq!(
            specs.iter().map(|spec| spec.seq).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "出参顺序由服务端按 seq 定死"
        );
        assert_eq!(rollup.weight_sum, dec("4"), "权重合计 1 + 2 + 1");
        assert_eq!(rollup.weighted_sum, dec("200"), "1×100 + 2×50 + 1×0");
        assert_eq!(rollup.pct_unrounded, dec("50"), "200 / 4");
        assert_eq!(rollup.pct, dec("50"), "回写对象级的就是这个 2 位值");

        // 逐行贡献相加 = 综合完工率（界面上「为什么汇总成 50%」要能一行行核）
        let contribution: Decimal = specs.iter().map(|spec| pct_dto::contribution_of(spec, &rollup)).sum();
        assert_eq!(contribution, dec("50"));

        // 权重占比：焊接 2/4 = 50%
        let welding = specs.iter().find(|spec| spec.seq == 2).expect("焊接");
        assert_eq!(pct_dto::share_of(welding, &rollup), dec("50"));
    }

    // ==================== 2. 权重改变约当，守恒不变 ====================

    #[test]
    fn w2_weights_move_the_pct_and_the_equivalents() {
        // 同样的工序完工率 100/50/0，只是权重从 (1,2,1) 换成 (3,1,1)
        let (_, lean) = three_ops();
        let (_, heavy) = rollup(vec![
            row(1, "下料", Some("3"), "100"),
            row(2, "焊接", Some("1"), "50"),
            row(3, "装配", Some("1"), "0"),
        ]);
        assert_eq!(lean.pct, dec("50"));
        assert_eq!(heavy.weighted_sum, dec("350"), "3×100 + 1×50 + 1×0");
        assert_eq!(heavy.pct, dec("70"), "350 / 5");

        // 完工 800 / 在制 200：材料约当不看完工率，加工约当随 P 变
        let wide = alloc::alloc_of(&alloc_facts(lean.pct, "800", "200", "0"));
        let narrow = alloc::alloc_of(&alloc_facts(heavy.pct, "800", "200", "0"));

        assert_eq!(wide.equiv_material, Some(dec("1000")), "材料：800 + 200 × 100%");
        assert_eq!(narrow.equiv_material, Some(dec("1000")), "权重动不了材料约当");

        assert_eq!(wide.equiv_conversion, Some(dec("900")), "800 + 200 × 50%");
        assert_eq!(narrow.equiv_conversion, Some(dec("940")), "800 + 200 × 70%");

        // 材料分配不受影响；人工 / 费用分母变大 ⇒ 分到在制的更多
        assert_eq!(element_of(&wide, Element::Material).wip, Some(dec("200")));
        assert_eq!(element_of(&narrow, Element::Material).wip, Some(dec("200")));

        let wide_labor = element_of(&wide, Element::Labor);
        let narrow_labor = element_of(&narrow, Element::Labor);
        assert_eq!(wide_labor.rate, Some(dec("1")), "900 / 900");
        assert_eq!(narrow_labor.rate, Some(dec("0.957447")), "900 / 940（6 位）");
        assert_eq!(narrow_labor.finished, Some(dec("765.9576")), "800 × 0.957447");
        assert_eq!(narrow_labor.wip, Some(dec("134.0424")), "900 − 765.9576（倒挤）");
        assert!(
            narrow_labor.wip.unwrap_or_default() > wide_labor.wip.unwrap_or_default(),
            "完工进度更高 ⇒ 更多成本转出、在制更少（{} vs {}）",
            narrow_labor.wip.unwrap_or_default(),
            wide_labor.wip.unwrap_or_default()
        );

        // 两种权重都必须分得干净
        assert!(wide.balanced(), "50% 口径下三要素守恒");
        assert!(narrow.balanced(), "70% 口径下三要素守恒");
        assert_eq!(
            narrow.finished_total.unwrap_or_default() + narrow.wip_total.unwrap_or_default(),
            dec("2350"),
            "完工 + 在制 = 本期投入（无期初）"
        );
    }

    // ==================== 3. 多工序只通过 completion_pct 影响约当 ====================

    #[test]
    fn w3_weighted_pct_is_the_only_bridge_to_the_equivalence() {
        // 五档综合完工率（含 0 与 100 两端）逐档与 calc 层公式对拍：
        // 只要 P 一致，约当就一致 —— 多工序没有「第二套约当」。
        for pct in ["33.33", "50", "66.67", "100"] {
            let (_, rollup) = {
                // 单工序把 P 直接给到对象级（等价于某组多工序汇总出同一个 P）
                rollup(vec![row(1, "合并工序", Some("1"), pct)])
            };
            let facts = equiv_facts(rollup.pct, "800", "200", "0");
            let eq = equiv::equiv_of(&facts);
            let (raw_material, raw_conversion) = item_dto::equivalents(dec("800"), dec("200"), rollup.pct);

            assert_eq!(
                eq.equiv_material,
                Some(equiv::eq_qty(raw_material)),
                "P={pct}：材料约当与 calc 逐位相等"
            );
            assert_eq!(
                eq.equiv_conversion,
                Some(equiv::eq_qty(raw_conversion)),
                "P={pct}：加工约当与 calc 逐位相等"
            );
            assert_eq!(eq.state, EquivState::Wip);

            // 分配表的分母必须是约当表的同一对数（不是自己再算一遍）
            let calc = alloc::alloc_of(&alloc_facts(rollup.pct, "800", "200", "0"));
            assert_eq!(calc.equiv_material, eq.equiv_material, "分配表材料分母 = 约当表");
            assert_eq!(calc.equiv_conversion, eq.equiv_conversion, "分配表加工分母 = 约当表");
            assert_eq!(calc.completed_qty, eq.completed_qty);
        }

        // 多工序汇总出的 P 与单工序直接填 P，走的是同一条路
        let (_, three) = three_ops();
        let from_ops = equiv::equiv_of(&equiv_facts(three.pct, "800", "200", "0"));
        let from_object = equiv::equiv_of(&equiv_facts(dec("50"), "800", "200", "0"));
        assert_eq!(from_ops, from_object, "P=50 时两侧结果必须一模一样");
    }

    // ==================== 4. 三要素分配（材料吃材料约当、工费吃加工约当） ====================

    #[test]
    fn w4_three_elements_split_and_conserve_under_the_multi_operation_rate() {
        let (_, rollup) = three_ops();
        let calc = alloc::alloc_of(&alloc_facts(rollup.pct, "800", "200", "0"));

        assert_eq!(calc.completed_qty, dec("800"));
        assert_eq!(calc.wip_qty, dec("200"));
        assert_eq!(calc.cur_total, dec("2350"));
        assert_eq!(calc.pool_total, Some(dec("2350")), "首期：池 = 本期投入");
        assert_eq!(calc.state, EquivState::Wip);
        assert!(calc.issues.is_empty(), "正常行不该有问题：{:?}", calc.issues);

        // 材料：1000 ÷ 1000 = 1 ⇒ 完工 800 / 在制 200
        let material = element_of(&calc, Element::Material);
        assert_eq!(material.equiv, Some(dec("1000")));
        assert_eq!(material.rate, Some(dec("1")));
        assert_eq!(material.finished, Some(dec("800")));
        assert_eq!(material.wip, Some(dec("200")));

        // 人工：900 ÷ 900 = 1 ⇒ 完工 800 / 在制 100
        let labor = element_of(&calc, Element::Labor);
        assert_eq!(labor.equiv, Some(dec("900")));
        assert_eq!(labor.rate, Some(dec("1")));
        assert_eq!(labor.finished, Some(dec("800")));
        assert_eq!(labor.wip, Some(dec("100")));

        // 制造费用：450 ÷ 900 = 0.5 ⇒ 完工 400 / 在制 50（尾差留在在制）
        let overhead = element_of(&calc, Element::Overhead);
        assert_eq!(overhead.equiv, Some(dec("900")), "与人工同一分母");
        assert_eq!(overhead.rate, Some(dec("0.5")));
        assert_eq!(overhead.finished, Some(dec("400")));
        assert_eq!(overhead.wip, Some(dec("50")));

        assert_eq!(calc.finished_total, Some(dec("2000")), "800 + 800 + 400");
        assert_eq!(calc.wip_total, Some(dec("350")), "200 + 100 + 50");
        assert!(calc.balanced(), "逐要素：完工 + 在制 = 池");
        assert_eq!(calc.unit_finished_cost, Some(dec("2.5")), "完工单位成本 = 2000 ÷ 800");
    }

    // ==================== 5. 余额表不另起公式 ====================

    #[test]
    fn w5_balance_rolls_the_allocation_without_a_second_formula() {
        let (_, rollup) = three_ops();
        let facts = alloc_facts(rollup.pct, "800", "200", "0");
        let calc = alloc::alloc_of(&facts);
        let roll = balance::balance_of(&facts, &calc);

        assert_eq!(roll.beg_qty, Decimal::ZERO, "首期没有期初数量");
        assert_eq!(roll.beg_total, Some(Decimal::ZERO), "首期期初金额 0（不是 null）");
        assert_eq!(roll.cur_total, Some(dec("2350")));
        assert_eq!(roll.finished_total, Some(dec("2000")));
        assert_eq!(roll.wip_total, Some(dec("350")));
        assert_eq!(roll.source, balance::BalanceSource::Live, "没结算 ⇒ 试算口径");
        assert!(roll.elements_available(), "三要素可用");

        // 逐要素：期初 + 投入 − 完工 = 期末，且与分配结果逐位相同
        for element in Element::ALL {
            let item = element_of(&calc, element);
            let line = roll.element_of(element);
            assert_eq!(line.beg, item.beg, "{element:?} 期初同源");
            assert_eq!(line.cur, Some(item.cur), "{element:?} 投入同源");
            assert_eq!(line.finished, item.finished, "{element:?} 完工同源");
            assert_eq!(line.wip, item.wip, "{element:?} 期末同源");
            assert_eq!(
                line.beg.unwrap_or_default() + line.cur.unwrap_or_default() - line.finished.unwrap_or_default(),
                line.wip.unwrap_or_default(),
                "{element:?} 期初 + 投入 − 转出 = 期末"
            );
        }
        assert_eq!(
            roll.element_of(Element::Material).wip.unwrap_or_default()
                + roll.element_of(Element::Labor).wip.unwrap_or_default()
                + roll.element_of(Element::Overhead).wip.unwrap_or_default(),
            roll.wip_total.unwrap_or_default(),
            "三要素加总 = 期末余额"
        );
    }

    // ==================== 6. 跨月：上期单位成本 → 本期期初三要素 ====================

    #[test]
    fn w6_prior_unit_costs_become_this_period_element_beginnings() {
        let facts = cross_month_facts();
        let calc = alloc::alloc_of(&facts);

        // 本期完工 = 累计 800 − 前期已结算 300 = 500（不能拿累计数当本期）
        assert_eq!(calc.completed_qty, dec("500"));
        assert_eq!(calc.equiv_material, Some(dec("700")), "500 + 200 × 100%");
        assert_eq!(calc.equiv_conversion, Some(dec("600")), "500 + 200 × 50%");

        // 期初还原：材料 5×100=500、人工 3×(100×50%)=150、费用吃尾差 750−650=100
        assert_eq!(calc.beg_total, Some(dec("750")), "= 上期期末在制成本，一分不差");
        let material = element_of(&calc, Element::Material);
        let labor = element_of(&calc, Element::Labor);
        let overhead = element_of(&calc, Element::Overhead);
        assert_eq!(material.beg, Some(dec("500")));
        assert_eq!(labor.beg, Some(dec("150")));
        assert_eq!(overhead.beg, Some(dec("100")));
        assert_eq!(
            material.beg.unwrap_or_default() + labor.beg.unwrap_or_default() + overhead.beg.unwrap_or_default(),
            dec("750"),
            "期初三要素加总 = 上期期末在制成本"
        );

        // 池 = 期初 + 本期投入；料 1400 / 工 1200 / 费 600
        assert_eq!(material.pool, Some(dec("1400")));
        assert_eq!(labor.pool, Some(dec("1200")));
        assert_eq!(overhead.pool, Some(dec("600")));
        assert_eq!(calc.pool_total, Some(dec("3200")), "750 + 2450");

        // 分配率：2 / 2 / 1 ⇒ 完工 2500、在制 700
        assert_eq!(material.rate, Some(dec("2")));
        assert_eq!(labor.rate, Some(dec("2")));
        assert_eq!(overhead.rate, Some(dec("1")));
        assert_eq!(calc.finished_total, Some(dec("2500")));
        assert_eq!(calc.wip_total, Some(dec("700")));
        assert!(calc.balanced());

        // 余额表同数
        let roll = balance::balance_of(&facts, &calc);
        assert_eq!(roll.beg_qty, dec("100"), "期初数量取上期期末在制");
        assert_eq!(roll.beg_total, Some(dec("750")));
        assert_eq!(roll.cur_total, Some(dec("2450")));
        assert_eq!(roll.finished_total, Some(dec("2500")));
        assert_eq!(roll.wip_total, Some(dec("700")));
    }

    // ==================== 7. 结转：已结算 → 下期期初 ====================

    #[test]
    fn w7_a_settled_balance_carries_into_the_next_period() {
        let mut facts = cross_month_facts();
        facts.stored = Some(settled_item());
        let calc = alloc::alloc_of(&facts);
        assert!(calc.drift.is_empty(), "快照与重算一致 ⇒ 不该报漂移：{:?}", calc.drift);

        let roll = balance::balance_of(&facts, &calc);
        assert_eq!(roll.source, balance::BalanceSource::Settled);
        assert_eq!(roll.wip_total, Some(dec("700")), "已结算 ⇒ 取账面快照");
        assert_eq!(roll.book.map(|book| book.wip_end_qty), Some(dec("200")));
        assert!(!roll.ledger_drift, "快照与试算一致 ⇒ 分要素照给");
        assert!(roll.elements_available());

        let batch = BatchRef {
            id: "b1".to_string(),
            batch_no: "CS-M524-0001".to_string(),
            status: "SETTLED".to_string(),
            reverse_reason: None,
        };
        let book = BookBalance {
            wip_end_qty: dec("200"),
            wip_end_cost: dec("700"),
        };

        // 下期期初与本期账面期末一分不差 ⇒ 已结转
        let next_exact = NextFacts {
            period_code: "2026-03".to_string(),
            wip_beg_qty: dec("200"),
            wip_beg_cost: dec("700"),
            item_status: "CALC".to_string(),
            period_status: "DRAFT".to_string(),
        };
        let carried = balance::carry_of("2026-02", &calc, Some(book), "OPEN", Some(&batch), Some(&next_exact));
        assert_eq!(carried.state, CarryState::Carried);
        assert_eq!(carried.diff_cost, Some(Decimal::ZERO));
        assert_eq!(carried.diff_qty, Some(Decimal::ZERO));
        assert_eq!(carried.to_period.as_deref(), Some("2026-03"));
        assert!(!carried.state.needs_attention(), "已结转不需要人处理");

        // 下一期期初少了一分钱 ⇒ 结转不齐，差额要报出来（不许静默）
        let next_short = NextFacts {
            wip_beg_cost: dec("699.99"),
            ..next_exact
        };
        let mismatch = balance::carry_of("2026-02", &calc, Some(book), "OPEN", Some(&batch), Some(&next_short));
        assert_eq!(mismatch.state, CarryState::Mismatch);
        assert_eq!(mismatch.diff_cost, Some(dec("-0.01")));
        assert!(mismatch.state.needs_attention());
        assert!(mismatch.hint.contains("结转不齐"), "{}", mismatch.hint);
    }

    // ==================== 8. 零进度：三张表一起 blocked、不许画 0 ====================

    #[test]
    fn w8_zero_progress_blocks_every_table_and_never_fakes_zero() {
        // 三道工序都填 0%（权重怎么配都一样）
        let (_, zero) = rollup(vec![
            row(1, "下料", Some("1"), "0"),
            row(2, "焊接", Some("2"), "0"),
            row(3, "装配", Some("1"), "0"),
        ]);
        assert_eq!(zero.pct, Decimal::ZERO);

        let facts = equiv_facts(zero.pct, "800", "200", "0");
        let eq = equiv::equiv_of(&facts);
        assert_eq!(eq.state, EquivState::Blocked);
        assert_eq!(eq.first_issue(), Some("WIP_PCT_MISSING"));
        assert_eq!(eq.equiv_conversion, None, "加工约当算不出来 ⇒ null，不是 0");
        assert_eq!(eq.equiv_material, Some(dec("1000")), "材料侧照算（在制按 100% 计料）");

        let alloc_facts = alloc_facts(zero.pct, "800", "200", "0");
        let calc = alloc::alloc_of(&alloc_facts);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.pool_total, Some(dec("2350")), "池是事实，照给");
        assert_eq!(calc.finished_total, None);
        assert_eq!(calc.wip_total, None);
        for element in Element::ALL {
            let item = element_of(&calc, element);
            assert_eq!(item.rate, None, "{element:?} 分配率不给");
            assert_eq!(item.finished, None, "{element:?} 完工金额不给（不画假 0）");
            assert_eq!(item.wip, None, "{element:?} 在制金额不给（不画假 0）");
        }

        let roll = balance::balance_of(&alloc_facts, &calc);
        assert_eq!(roll.wip_total, None, "余额算不出来 ⇒ null");
        assert!(!roll.elements_available(), "分要素一律不给");

        let carry = balance::carry_of("2026-02", &calc, None, "OPEN", None, None);
        assert_eq!(carry.state, CarryState::Blocked, "连余额都没有 ⇒ 谈不上结转");
        assert!(carry.state.needs_attention());

        // 合计：坏行不进金额（否则合计会像「这些在制不存在」）
        let mut totals = alloc::AllocTotals::default();
        totals.add(&calc);
        assert_eq!(totals.row_count, 1);
        assert_eq!(totals.blocked_row_count, 1);
        assert_eq!(totals.wip_total, Decimal::ZERO, "坏行不参与金额合计");
        assert_eq!(totals.wip_qty, dec("200"), "数量是事实，照进合计");
    }

    // ==================== 9. 全部完工：整池进完工 ====================

    #[test]
    fn w9_finished_batch_pushes_the_whole_pool_into_finished_goods() {
        // 工序全 100%，且期末在制是 0（跨月结清的那一期）
        let (_, done) = rollup(vec![
            row(1, "下料", Some("1"), "100"),
            row(2, "焊接", Some("2"), "100"),
            row(3, "装配", Some("1"), "100"),
        ]);
        assert_eq!(done.pct, dec("100"));

        let facts = alloc_facts(done.pct, "800", "0", "0");
        let calc = alloc::alloc_of(&facts);
        assert_eq!(calc.state, EquivState::Finished);
        assert_eq!(calc.equiv_material, Some(dec("800")));
        assert_eq!(calc.equiv_conversion, Some(dec("800")), "没有在制 ⇒ 两个约当相等");
        assert_eq!(calc.finished_total, Some(dec("2350")), "整池（含尾差）进完工");
        assert_eq!(calc.wip_total, Some(Decimal::ZERO));
        assert!(calc.balanced());
        // 无在制时整池进完工，不按分配率乘（尾差留在完工侧）
        assert_eq!(element_of(&calc, Element::Overhead).wip, Some(Decimal::ZERO));

        let roll = balance::balance_of(&facts, &calc);
        assert_eq!(roll.wip_total, Some(Decimal::ZERO));
        let carry = balance::carry_of("2026-02", &calc, None, "OPEN", None, None);
        assert_eq!(carry.state, CarryState::Cleared, "无余额可结转");
        assert!(!carry.state.needs_attention());

        // 约当表同样给 finished 形状
        let eq = equiv::equiv_of(&equiv_facts(done.pct, "800", "0", "0"));
        assert_eq!(eq.state, EquivState::Finished);
        assert_eq!(eq.equiv_material, eq.equiv_conversion);
    }

    // ==================== 10. 链上用的是回写后的 2 位完工率 ====================

    #[test]
    fn w10_the_chain_uses_the_two_decimal_pct_the_operations_wrote_back() {
        // (2×33.34 + 1×0) / 3 = 66.68 / 3 = 22.2266666…
        let (_, rollup) = rollup(vec![
            row(1, "粗加工", Some("2"), "33.34"),
            row(2, "精加工", Some("1"), "0"),
        ]);
        assert_eq!(rollup.weighted_sum, dec("66.68"));
        assert_eq!(rollup.pct_unrounded, dec("22.226667"), "未舍入商只用来解释尾差（6 位）");
        assert_eq!(rollup.pct, dec("22.23"), "回写对象级的是 2 位（四舍五入取远）");

        // 链上的加工约当用 22.23，不用 22.226667：口径只能有一处，否则余额与计算单差尾
        let eq = equiv::equiv_of(&equiv_facts(rollup.pct, "100", "100", "0"));
        assert_eq!(eq.equiv_conversion, Some(dec("122.23")), "100 + 100 × 22.23%");
        assert_eq!(eq.equiv_material, Some(dec("200")));

        // 若有人把未舍入商当分母，加工约当会是 122.226667 —— 与本卡口径差 0.003333。
        // 用 calc 的同一段算式把「差多少」钉出来，避免后来人又改成未舍入口径。
        let (_, raw_conversion) = item_dto::equivalents(dec("100"), dec("100"), dbg_pct_unrounded());
        assert_eq!(raw_conversion.round_dp(6), dec("122.226667"));
        assert!(
            raw_conversion < dec("122.23"),
            "未舍入商 22.226667% 与回写值 22.23% 差 0.003333：链上只认回写值，混用会让余额与计算单差尾"
        );
    }

    /// 「未舍入商」的对照值（保持算式单源：直接调 `rollup_of` 的同一个纯函数）。
    fn dbg_pct_unrounded() -> Decimal {
        let (_, rollup) = rollup(vec![
            row(1, "粗加工", Some("2"), "33.34"),
            row(2, "精加工", Some("1"), "0"),
        ]);
        rollup.pct_unrounded
    }

    // ==================== 11. 录入侧提醒在链路上仍成立 ====================

    #[test]
    fn w11_operations_warnings_survive_the_multi_operation_case() {
        let (specs, rollup) = three_ops();

        // 正常情形（在制 200、对象级与汇总一致）⇒ 不该有提醒
        assert!(
            pct_dto::warnings_of(dec("200"), rollup.pct, &specs, Some(&rollup)).is_empty(),
            "多工序正常录入不该有提醒"
        );

        // 全 0 进度 + 有在制 ⇒ 提醒「会被 66067 拦下」
        let (zero_specs, zero) = rollup_of(vec![row(1, "下料", Some("1"), "0"), row(2, "焊接", Some("2"), "0")]);
        let warnings = pct_dto::warnings_of(dec("200"), zero.pct, &zero_specs, Some(&zero));
        assert!(
            warnings.iter().any(|text| text.contains("66067")),
            "在制 > 0 而汇总 0% 必须提醒：{warnings:?}"
        );

        // 在制为 0 ⇒ 提醒「完工率不参与约当」（不是拦，是提示回来更新）
        let idle = pct_dto::warnings_of(Decimal::ZERO, rollup.pct, &specs, Some(&rollup));
        assert!(
            idle.iter().any(|text| text.contains("不参与约当产量")),
            "在制为 0 要提醒：{idle:?}"
        );

        // 权重全相同 ⇒ 提醒「就是算术平均」
        let (flat_specs, flat) = rollup_of(vec![row(1, "下料", Some("1"), "100"), row(2, "焊接", Some("1"), "50")]);
        let flat_warnings = pct_dto::warnings_of(dec("200"), flat.pct, &flat_specs, Some(&flat));
        assert!(
            flat_warnings.iter().any(|text| text.contains("权重相同")),
            "权重全相同要提醒：{flat_warnings:?}"
        );

        // 对象级被人直接改过 ⇒ 提醒「保存会回写成汇总值」
        let drift_warnings = pct_dto::warnings_of(dec("200"), dec("80"), &specs, Some(&rollup));
        assert!(
            drift_warnings.iter().any(|text| text.contains("不一致")),
            "对象级 ≠ 汇总要提醒：{drift_warnings:?}"
        );
    }

    /// `w11` 用的小工具：归一化 + 汇总（与 `rollup` 同一条路径，单独命名便于读）。
    fn rollup_of(rows: Vec<OperationRowReq>) -> (Vec<OperationSpec>, Rollup) {
        rollup(rows)
    }

    // ==================== 12. 单工序：权重不起作用 ====================

    #[test]
    fn w12_single_operation_weights_never_move_its_own_pct() {
        let (_, light) = rollup(vec![row(1, "唯一工序", Some("1"), "37.5")]);
        let (_, heavy) = rollup(vec![row(1, "唯一工序", Some("9"), "37.5")]);
        assert_eq!(light.pct, dec("37.5"));
        assert_eq!(heavy.pct, dec("37.5"), "单工序权重不起作用");
        assert_eq!(light.pct_unrounded, heavy.pct_unrounded);

        // 两道工序起，权重就说话：(1×100 + 3×0) / 4 = 25%
        let (_, two) = rollup(vec![row(1, "前道", Some("1"), "100"), row(2, "后道", Some("3"), "0")]);
        assert_eq!(two.pct, dec("25"));
        let calc = alloc::alloc_of(&alloc_facts(two.pct, "800", "200", "0"));
        assert_eq!(calc.equiv_conversion, Some(dec("850")), "800 + 200 × 25%");
    }

    // ==================== 13. 三张表同一个权限码 ====================

    #[test]
    fn w13_the_three_wip_tables_share_one_permission_code() {
        let expected = "business:erp:cost-object:list";
        assert_eq!(equiv::PERM_LIST, expected);
        assert_eq!(alloc::PERM_LIST, expected);
        assert_eq!(balance::PERM_LIST, expected);
        // 工序表是录入面：读 list、写 update，不给「工序专属码」
        assert_eq!(pct_dto::PERM_READ, expected);
        assert_eq!(pct_dto::PERM_WRITE, "business:erp:cost-object:update");
        assert_eq!(pct_dto::PERMISSIONS, [pct_dto::PERM_READ, pct_dto::PERM_WRITE]);
    }

    // ==================== 14. 两行合计：三张表口径一致 ====================

    #[test]
    fn w14_totals_of_two_rows_keep_the_three_element_identity() {
        let (_, rollup) = three_ops();
        // A 行：有在制（800 / 200 @ 50%，料 1000 / 工 900 / 费 450）
        let facts_a = alloc_facts(rollup.pct, "800", "200", "0");
        // B 行：全部完工（100 / 0），投入 100 / 100 / 100
        let mut facts_b = alloc_facts(Decimal::ZERO, "100", "0", "0");
        facts_b.mat_cur = dec("100");
        facts_b.labor_cur = dec("100");
        facts_b.oh_cur = dec("100");

        let calc_a = alloc::alloc_of(&facts_a);
        let calc_b = alloc::alloc_of(&facts_b);
        assert_eq!(calc_b.state, EquivState::Finished);

        let mut eq_totals = equiv::EquivTotals::default();
        eq_totals.add(&facts_a.equiv_facts(), &equiv::equiv_of(&facts_a.equiv_facts()));
        eq_totals.add(&facts_b.equiv_facts(), &equiv::equiv_of(&facts_b.equiv_facts()));
        assert_eq!(eq_totals.row_count, 2);
        assert_eq!(eq_totals.wip_row_count, 1);
        assert_eq!(eq_totals.finished_row_count, 1);
        assert_eq!(eq_totals.equiv_material, dec("1100"), "1000 + 100");
        assert_eq!(eq_totals.equiv_conversion, dec("1000"), "900 + 100");

        let mut alloc_totals = alloc::AllocTotals::default();
        alloc_totals.add(&calc_a);
        alloc_totals.add(&calc_b);
        assert_eq!(alloc_totals.pool_total, dec("2650"), "2350 + 300");
        assert_eq!(alloc_totals.finished_total, dec("2300"), "2000 + 300");
        assert_eq!(alloc_totals.wip_total, dec("350"), "350 + 0");

        let mut balance_totals = balance::BalanceTotals::default();
        for (facts, calc) in [(&facts_a, &calc_a), (&facts_b, &calc_b)] {
            let roll = balance::balance_of(facts, calc);
            let carry = balance::carry_of("2026-02", calc, None, "OPEN", None, None);
            balance_totals.add(calc, &roll, &carry);
        }
        assert_eq!(balance_totals.row_count, 2);
        assert_eq!(balance_totals.unsettled_count, 1, "A 行有余额没入账");
        assert_eq!(balance_totals.cleared_count, 1, "B 行无余额");
        assert_eq!(balance_totals.wip_total, dec("350"));
        assert_eq!(balance_totals.wip_material, dec("200"));
        assert_eq!(balance_totals.wip_labor, dec("100"));
        assert_eq!(balance_totals.wip_overhead, dec("50"));
        assert_eq!(balance_totals.element_row_count, 2);
        // 三张表同一个余额
        assert_eq!(balance_totals.wip_total, alloc_totals.wip_total);
        assert_eq!(
            balance_totals.wip_total,
            balance_totals.wip_material + balance_totals.wip_labor + balance_totals.wip_overhead,
            "余额合计 = 三要素合计"
        );
        assert_eq!(
            balance_totals.beg_total + balance_totals.cur_total,
            dec("0") + dec("2650")
        );
        assert_eq!(
            balance_totals.beg_total + balance_totals.cur_total,
            balance_totals.finished_total + balance_totals.wip_total,
            "期初 + 投入 = 完工 + 期末（整表级恒等式）"
        );
    }
}

// ==================== B. 真库全链路 ====================

#[cfg(test)]
mod db {
    use super::*;

    use crate::business::changelog::ChangeCtx;
    use crate::business::erp::cost::cost_period_item_dto::{self as item_dto, BegCost, ItemInput, OutputInput};
    use crate::business::erp::cost::cost_wip_alloc_dto::{self as alloc, AllocQuery, AllocRes, AllocRow};
    use crate::business::erp::cost::cost_wip_alloc_handler::{alloc_table_of, filter_of as alloc_filter};
    use crate::business::erp::cost::cost_wip_balance_dto::{BalanceQuery, BalanceRes, BalanceRow};
    use crate::business::erp::cost::cost_wip_balance_handler::{balance_table_of, filter_of as balance_filter};
    use crate::business::erp::cost::cost_wip_equiv_dto::{EquivQuery, EquivRes, EquivRow};
    use crate::business::erp::cost::cost_wip_equiv_handler::{equiv_table_of, filter_of as equiv_filter};
    use crate::business::erp::cost::cost_wip_pct_dto::{OperationRowReq, OperationSaveReq};
    use crate::business::erp::cost::cost_wip_pct_handler::save_operations;
    use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
    use cloudpivot_kernel::business::shared::scope::ScopeKind;
    use nik::prelude::rust_decimal::Decimal;
    use nik::uuid::Uuid;
    use std::str::FromStr;
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数量字面量无法解析：{text}"))
    }

    /// 出参里的数字是字符串：按 Decimal 比，避免「1000 vs 1000.0000」的标度假红。
    fn num(text: impl AsRef<str>) -> Decimal {
        let text = text.as_ref();
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数字字符串无法解析：{text}"))
    }

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// 审计上下文（操作人固定一个随机 id：`sys_change_log.operator_id` 不查外键）。
    fn ctx() -> ChangeCtx<'static> {
        ChangeCtx::api(Some(Uuid::now_v7()), Some("M5-24 夹具"))
    }

    struct Fixture {
        suffix: String,
        /// 三个连续期间 01 / 02 / 03（跨月结转要两期）。
        periods: [String; 3],
        dept: Uuid,
        dept_name: String,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        batches: std::sync::Mutex<Vec<Uuid>>,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 2000 + (slot % 70) as i32;
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
            ];
            Self::reset(&periods).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let product = crate::test_seed::ensure_material_kind(
                &format!("M524-MAT-{suffix}"),
                &format!("M5-24 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M524W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-24 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                product,
                warehouse,
                objects: std::sync::Mutex::new(Vec::new()),
                batches: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M524-{}", self.tag(name))
        }

        /// 造成本对象（累计完工 / 期末在制 / 对象级完工率 / 状态由用例给）。
        async fn object(&self, name: &str, status: &str, completed: &str, wip: &str, pct: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  completion_pct, material_input_mode, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 800, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, 'ONE_TIME', \
                             ?::uuid, ?, '{}-01', ?, {closed_at}, now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&self.product),
                    Value::Text(format!(
                        "LOT-M524-{name}-{}",
                        crate::test_seed::last_chars(&self.suffix, 6)
                    )),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("objects").push(id);
            id
        }

        /// 造一条归集行（要素 / 金额 / 期间由用例给）。
        async fn collect(&self, object: Uuid, element: &str, amount: &str, period: &str) {
            let (material, qty) = if element == "MATERIAL" {
                ("?::uuid", "10")
            } else {
                ("NULL", "NULL")
            };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&object),
                Value::Text(period.to_string()),
                Value::Text(element.to_string()),
                Value::Text(format!("{period}-10")),
            ];
            if element == "MATERIAL" {
                params.push(Value::from(&self.product));
            }
            params.push(Value::Text(amount.to_string()));
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                  line_no, occur_date, material_id, qty, amount, \
                                                  remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, {material}, \
                             {qty}, ?::text::numeric, 'M5-24 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集行夹具");
        }

        /// 造一张成本核算批次（`SETTLED` 要挂审批 + 结算时间 + 完工凭证，库层硬约束）。
        async fn period_batch(&self, period: &str, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let settled = if status == "SETTLED" { "now()" } else { "NULL" };
            let approval = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let voucher = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let mut params = vec![
                Value::from(&id),
                Value::Text(period.to_string()),
                Value::Text(format!("CS-M524-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                Value::Text(status.to_string()),
            ];
            if status == "SETTLED" {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period (id, period_code, batch_no, status, \
                                                  approval_instance_id, settled_at, finish_voucher_id, \
                                                  created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?, {approval}, {settled}, {voucher}, now(), now())"
                ),
                params,
            )
            .await
            .expect("造成本批次夹具");
            self.batches.lock().expect("batches").push(id);
            id
        }

        /// 用 **真 calc 的 `compose`** 算一张计算单再落库（不是手抄数字）。
        #[allow(clippy::too_many_arguments)]
        async fn calc_item(
            &self,
            object: Uuid,
            batch: Uuid,
            status: &str,
            period: &str,
            completed: &str,
            wip: &str,
            pct: &str,
            mat: &str,
            labor: &str,
            oh: &str,
        ) -> item_dto::ItemCalc {
            let input = ItemInput {
                period_code: period.to_string(),
                cost_object_id: object,
                object_no: format!("{object:?}"),
                product_id: self.product,
                output: OutputInput {
                    cumulative_completed_qty: dec(completed),
                    prior_completed_qty: Decimal::ZERO,
                    wip_end_qty: dec(wip),
                    completion_pct: dec(pct),
                    prior_wip_qty: Decimal::ZERO,
                    material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
                },
                beg: BegCost::default(),
                mat_cur: dec(mat),
                labor_cur: dec(labor),
                oh_cur: dec(oh),
            };
            let calc = item_dto::compose(&input).expect("夹具计算单必须可算（compose 拒绝了输入）");
            let stock_in = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&batch),
                Value::from(&object),
                Value::from(&self.product),
                Value::Text(calc.wip_beg_qty.to_string()),
                Value::Text(calc.wip_beg_cost.to_string()),
                Value::Text(calc.completed_qty.to_string()),
                Value::Text(calc.wip_end_qty.to_string()),
                Value::Text(calc.completion_pct.to_string()),
                Value::Text(calc.equiv_material.to_string()),
                Value::Text(calc.equiv_conversion.to_string()),
                Value::Text(calc.mat_cur.to_string()),
                Value::Text(calc.labor_cur.to_string()),
                Value::Text(calc.oh_cur.to_string()),
                Value::Text(calc.unit_material.to_string()),
                Value::Text(calc.unit_labor.to_string()),
                Value::Text(calc.unit_overhead.to_string()),
                Value::Text(calc.finished_cost.to_string()),
                Value::Text(calc.wip_end_cost.to_string()),
                Value::Text(calc.unit_finished_cost.to_string()),
                Value::Text(status.to_string()),
            ];
            if status == "SETTLED" {
                params.push(Value::from(&Uuid::now_v7()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                        wip_beg_qty, wip_beg_cost, completed_qty, \
                                                        wip_end_qty, completion_pct, equiv_material, \
                                                        equiv_conversion, mat_cur, labor_cur, oh_cur, \
                                                        unit_material, unit_labor, unit_overhead, \
                                                        finished_cost, wip_end_cost, unit_finished_cost, \
                                                        status, stock_in_id, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?, {stock_in}, now(), now())"
                ),
                params,
            )
            .await
            .expect("造计算单夹具");
            calc
        }

        /// 对象级完工率（真库值，不信出参）。
        async fn object_pct(&self, object: Uuid) -> Decimal {
            db().scalar_with_params::<Decimal>(
                "SELECT completion_pct FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("查对象级完工率")
        }

        /// 清掉该槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let first = &periods[0];
            let third = &periods[2];
            for statement in [
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-24 夹具' AND period_code BETWEEN ? AND ?",
                // 审计要**先**删：上一次运行如果在断言处 panic，工序行还在；本轮的
                // `DELETE FROM biz_cost_object_operation` 会把它删掉，审计就成了孤儿
                // （M5-21 的 `zz_teardown` 会按全库数孤儿审计 ⇒ 别的卡被迫红）。
                "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object_operation' \
                   AND resource_id IN (SELECT id::text FROM biz_cost_object_operation \
                                        WHERE cost_object_id IN \
                                          (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M524-%' \
                                             AND start_date BETWEEN (? || '-01')::date \
                                                 AND ((? || '-01')::date + interval '3 months')))",
                "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object' \
                   AND resource_id IN (SELECT id::text FROM biz_cost_object WHERE object_no LIKE 'CB-M524-%' \
                                         AND start_date BETWEEN (? || '-01')::date \
                                             AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_object_operation WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M524-%' \
                      AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M524-%' \
                      AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M524-%' \
                    AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')",
            ] {
                let params = if statement.contains("remark") {
                    vec![Value::Text(first.clone()), Value::Text(third.clone())]
                } else {
                    vec![Value::Text(first.clone()), Value::Text(first.clone())]
                };
                let _ = db().exec_sql(statement.to_string(), params).await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M524-%' \
                        AND period_code BETWEEN ? AND ?"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(third.clone())],
                )
                .await;
        }

        /// 清场（FK 顺序：工序 / 审计 → 计算单 → 归集 → 批次 → 对象）。
        async fn cleanup(&self) {
            let objects = self.objects.lock().expect("objects").clone();
            let batches = self.batches.lock().expect("batches").clone();
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if objects.is_empty() {
                return;
            }
            let objects_sql = list(&objects);
            let plain = objects.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(",");
            for statement in [
                format!(
                    "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object_operation' \
                       AND resource_id IN (SELECT id::text FROM biz_cost_object_operation \
                                            WHERE cost_object_id IN ({objects_sql}))"
                ),
                format!(
                    "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object' \
                       AND resource_id IN ({plain})"
                ),
                format!("DELETE FROM biz_cost_object_operation WHERE cost_object_id IN ({objects_sql})"),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({objects_sql})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects_sql})"),
            ] {
                db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
            }
            if !batches.is_empty() {
                db().exec_sql(
                    format!("DELETE FROM biz_cost_period WHERE id IN ({})", list(&batches)),
                    vec![],
                )
                .await
                .expect("清场（期间批次）");
            }
            db().exec_sql(
                format!("DELETE FROM biz_cost_object WHERE id IN ({objects_sql})"),
                vec![],
            )
            .await
            .expect("清场（成本对象）");
        }

        /// 本夹具残留行数（按对象号后缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            let pattern = Value::Text(format!("%{}", self.tag("")));
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE ?) + \
                        (SELECT count(*) FROM biz_cost_object_operation WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) + \
                        (SELECT count(*) FROM biz_cost_period_item WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) + \
                        (SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![pattern.clone(), pattern.clone(), pattern.clone(), pattern],
            )
            .await
            .expect("查残留")
        }
    }

    // ---------- 工序入参 ----------

    fn op_row(seq: i32, name: &str, weight: &str, pct: &str) -> OperationRowReq {
        OperationRowReq {
            seq: Some(seq),
            operation_name: Some(name.to_string()),
            weight: Some(dec(weight)),
            completion_pct: Some(dec(pct)),
            remark: None,
        }
    }

    /// 三道工序：下料 100%（1）/ 焊接 50%（2）/ 装配 0%（1）⇒ 综合 50%。
    fn standard_rows() -> Vec<OperationRowReq> {
        vec![
            op_row(1, "下料", "1", "100"),
            op_row(2, "焊接", "2", "50"),
            op_row(3, "装配", "1", "0"),
        ]
    }

    async fn save_ops(
        principal: &ScopedPrincipal,
        object: Uuid,
        rows: Vec<OperationRowReq>,
    ) -> crate::business::erp::cost::cost_wip_pct_dto::OperationRes {
        let ctx = ctx();
        save_operations(principal, &ctx, &object.to_string(), OperationSaveReq { rows })
            .await
            .expect("保存工序完工率")
    }

    // ---------- 三张表（走真实 handler） ----------

    async fn equiv_sheet(principal: &ScopedPrincipal, period: &str, tag: &str) -> EquivRes {
        let query = EquivQuery {
            period_code: Some(period.to_string()),
            keyword: Some(tag.to_string()),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
        };
        let filter = equiv_filter(&query).expect("入参归一化");
        equiv_table_of(principal, &filter).await.expect("约当产量表")
    }

    async fn alloc_sheet(principal: &ScopedPrincipal, period: &str, tag: &str) -> AllocRes {
        let query = AllocQuery {
            period_code: Some(period.to_string()),
            keyword: Some(tag.to_string()),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
        };
        let filter = alloc_filter(&query).expect("入参归一化");
        alloc_table_of(principal, &filter).await.expect("在制品分配表")
    }

    async fn balance_sheet(principal: &ScopedPrincipal, period: &str, tag: &str) -> BalanceRes {
        let query = BalanceQuery {
            period_code: Some(period.to_string()),
            keyword: Some(tag.to_string()),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
        };
        let filter = balance_filter(&query).expect("入参归一化");
        balance_table_of(principal, &filter).await.expect("期末在制余额表")
    }

    fn equiv_row_of<'a>(sheet: &'a EquivRes, object_no: &str) -> &'a EquivRow {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("约当表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    fn alloc_row_of<'a>(sheet: &'a AllocRes, object_no: &str) -> &'a AllocRow {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("分配表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    fn balance_row_of<'a>(sheet: &'a BalanceRes, object_no: &str) -> &'a BalanceRow {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("余额表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    fn alloc_element(
        row: &AllocRow,
        element: alloc::Element,
    ) -> &crate::business::erp::cost::cost_wip_alloc_dto::ElementView {
        row.elements
            .iter()
            .find(|view| view.element == element.as_str())
            .unwrap_or_else(|| panic!("分配行缺要素 {}", element.as_str()))
    }

    fn balance_element(
        row: &BalanceRow,
        element: alloc::Element,
    ) -> &crate::business::erp::cost::cost_wip_balance_dto::ElementRollView {
        row.elements
            .iter()
            .find(|view| view.element == element.as_str())
            .unwrap_or_else(|| panic!("余额行缺要素 {}", element.as_str()))
    }

    /// 逐要素守恒（出参侧：完工 + 在制 = 池）。
    fn assert_balanced(row: &AllocRow) {
        let mut counted = 0;
        for element in alloc::Element::ALL {
            let item = alloc_element(row, element);
            let pool = num(item.pool.as_deref().expect("池必须有值"));
            let finished = num(item.finished.as_deref().expect("完工必须有值"));
            let wip = num(item.wip.as_deref().expect("在制必须有值"));
            assert_eq!(finished + wip, pool, "{} 完工 + 在制 = 池", element.label());
            assert!(item.balanced, "{} 出参自报守恒", element.label());
            counted += 1;
        }
        assert_eq!(counted, 3, "三要素一个都不能少");
    }

    // ==================== 15. 主链路：多工序 → 约当 → 分配 → 余额 ====================

    #[tokio::test]
    async fn d1_multi_operation_pct_drives_equivalence_allocation_and_balance() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();
        // 累计完工 800 / 期末在制 200；对象级完工率先留 0，靠工序表回写
        let object = f.object("D1", "OPEN", "800", "200", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "LABOR", "900", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "450", &f.periods[0]).await;

        let saved = save_ops(&principal, object, standard_rows()).await;
        assert_eq!(saved.rows.len(), 3, "三道工序都落地");
        let rollup = saved.rollup.expect("三道工序必须有汇总");
        assert_eq!(rollup.pct, "50", "(1×100 + 2×50 + 1×0) / 4");
        assert!(rollup.matched, "保存后对象级与汇总必须一致");
        assert_eq!(f.object_pct(object).await, dec("50"), "真库值：对象级已回写");

        // ---- 约当表 ----
        let eq_sheet = equiv_sheet(&principal, &f.periods[0], &f.tag("D1")).await;
        let eq_row = equiv_row_of(&eq_sheet, &f.object_no("D1"));
        assert_eq!(num(&eq_row.completed_qty), dec("800"));
        assert_eq!(num(&eq_row.completion_pct), dec("50"));
        assert_eq!(eq_row.wip_equiv_material.as_deref().map(num), Some(dec("200")));
        assert_eq!(eq_row.equiv_material.as_deref().map(num), Some(dec("1000")));
        assert_eq!(eq_row.wip_equiv_conversion.as_deref().map(num), Some(dec("100")));
        assert_eq!(eq_row.equiv_conversion.as_deref().map(num), Some(dec("900")));
        assert_eq!(eq_row.state, "wip");

        // ---- 分配表 ----
        let alloc_sheet_v = alloc_sheet(&principal, &f.periods[0], &f.tag("D1")).await;
        let row = alloc_row_of(&alloc_sheet_v, &f.object_no("D1"));
        assert_eq!(num(&row.cur_total), dec("2350"), "1000 + 900 + 450");
        assert_eq!(row.pool_total.as_deref().map(num), Some(dec("2350")));
        assert_eq!(row.equiv_material.as_deref().map(num), Some(dec("1000")));
        assert_eq!(row.equiv_conversion.as_deref().map(num), Some(dec("900")));
        assert_eq!(row.finished_total.as_deref().map(num), Some(dec("2000")));
        assert_eq!(row.wip_total.as_deref().map(num), Some(dec("350")));
        assert_balanced(row);
        assert_eq!(
            num(alloc_element(row, alloc::Element::Material).rate.as_deref().unwrap()),
            dec("1")
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Labor).rate.as_deref().unwrap()),
            dec("1")
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Overhead).rate.as_deref().unwrap()),
            dec("0.5")
        );

        // ---- 余额表 ----
        let bal_sheet = balance_sheet(&principal, &f.periods[0], &f.tag("D1")).await;
        let bal = balance_row_of(&bal_sheet, &f.object_no("D1"));
        assert_eq!(num(&bal.beg_qty), Decimal::ZERO);
        assert_eq!(bal.beg_total.as_deref().map(num), Some(Decimal::ZERO));
        assert_eq!(bal.cur_total.as_deref().map(num), Some(dec("2350")));
        assert_eq!(bal.finished_total.as_deref().map(num), Some(dec("2000")));
        assert_eq!(
            bal.wip_total.as_deref().map(num),
            Some(dec("350")),
            "期初 0 + 投入 2350 − 完工 2000"
        );
        assert_eq!(
            num(balance_element(bal, alloc::Element::Material).wip.as_deref().unwrap()),
            dec("200")
        );
        assert_eq!(
            num(balance_element(bal, alloc::Element::Labor).wip.as_deref().unwrap()),
            dec("100")
        );
        assert_eq!(
            num(balance_element(bal, alloc::Element::Overhead).wip.as_deref().unwrap()),
            dec("50")
        );
        assert_eq!(bal.source, "live", "没结算 ⇒ 试算口径");
        assert_eq!(bal.carry.state, "unsettled", "有余额但没入账");
        assert!(bal.elements_available);

        // ---- 三张表的合计同数 ----
        assert_eq!(num(&eq_sheet.totals.equiv_material), dec("1000"));
        assert_eq!(num(&alloc_sheet_v.totals.wip_total), dec("350"));
        assert_eq!(num(&bal_sheet.totals.wip_total), dec("350"));
        assert_eq!(num(&bal_sheet.totals.wip_material), dec("200"));
        assert_eq!(num(&bal_sheet.totals.wip_labor), dec("100"));
        assert_eq!(num(&bal_sheet.totals.wip_overhead), dec("50"));
        assert_eq!(
            num(&bal_sheet.totals.wip_material)
                + num(&bal_sheet.totals.wip_labor)
                + num(&bal_sheet.totals.wip_overhead),
            num(&bal_sheet.totals.wip_total),
            "余额合计 = 三要素合计"
        );

        f.cleanup().await;
    }

    // ==================== 16. 跨月：上期已结算 → 本期期初 ====================

    #[tokio::test]
    async fn d2_prior_settled_period_feeds_the_beginnings_through_the_operations() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();
        // 累计完工 800（上期已结算 300）⇒ 本期完工 500；在制 200
        let object = f.object("D2", "OPEN", "800", "200", "0").await;
        f.collect(object, "MATERIAL", "900", &f.periods[1]).await;
        f.collect(object, "LABOR", "1050", &f.periods[1]).await;
        f.collect(object, "OVERHEAD", "500", &f.periods[1]).await;

        // 上期（01）已结算：完工 300 / 在制 100 @50%；料 2000 / 工 1050 / 费 700
        // ⇒ 约当 400 / 350 ⇒ 单位成本 5 / 3 / 2 ⇒ 期末在制 500 + 150 + 100 = 750
        //
        // 顺序要紧：先录本期多工序（对象级回写 50%），再造上期已结算批次 ——
        // 该批一旦参与过已结算期间，工序表会被状态闸拦成 409 + 66070（M5-21 的判据）。
        let saved = save_ops(&principal, object, standard_rows()).await;
        assert_eq!(saved.rollup.expect("有汇总").pct, "50", "本期多工序 ⇒ 50%");

        let prior_batch = f.period_batch(&f.periods[0], "SETTLED").await;
        let prior = f
            .calc_item(
                object,
                prior_batch,
                "SETTLED",
                &f.periods[0],
                "300",
                "100",
                "50",
                "2000",
                "1050",
                "700",
            )
            .await;
        assert_eq!(prior.unit_material, dec("5"));
        assert_eq!(prior.unit_labor, dec("3"));
        assert_eq!(prior.unit_overhead, dec("2"));
        assert_eq!(prior.wip_end_cost, dec("750"));

        // ---- 约当表：本期完工扣掉前期已结算 ----
        let eq_sheet = equiv_sheet(&principal, &f.periods[1], &f.tag("D2")).await;
        let eq_row = equiv_row_of(&eq_sheet, &f.object_no("D2"));
        assert_eq!(num(&eq_row.completed_qty), dec("500"), "800 − 300");
        assert_eq!(eq_row.equiv_material.as_deref().map(num), Some(dec("700")));
        assert_eq!(eq_row.equiv_conversion.as_deref().map(num), Some(dec("600")));

        // ---- 分配表：期初按上期单位成本还原到三要素 ----
        let alloc_sheet_v = alloc_sheet(&principal, &f.periods[1], &f.tag("D2")).await;
        let row = alloc_row_of(&alloc_sheet_v, &f.object_no("D2"));
        let source = row.prior.as_ref().expect("有已结算上期就该有期初来源");
        assert_eq!(source.period_code, f.periods[0]);
        assert_eq!(num(&source.wip_end_cost), dec("750"));
        assert_eq!(row.beg_total.as_deref().map(num), Some(dec("750")));
        assert_eq!(
            num(alloc_element(row, alloc::Element::Material).beg.as_deref().unwrap()),
            dec("500")
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Labor).beg.as_deref().unwrap()),
            dec("150")
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Overhead).beg.as_deref().unwrap()),
            dec("100")
        );
        assert_eq!(row.pool_total.as_deref().map(num), Some(dec("3200")), "750 + 2450");
        assert_eq!(row.finished_total.as_deref().map(num), Some(dec("2500")));
        assert_eq!(row.wip_total.as_deref().map(num), Some(dec("700")));
        assert_balanced(row);

        // ---- 余额表：期初数量 / 金额一起滚 ----
        let bal_sheet = balance_sheet(&principal, &f.periods[1], &f.tag("D2")).await;
        let bal = balance_row_of(&bal_sheet, &f.object_no("D2"));
        assert_eq!(num(&bal.beg_qty), dec("100"), "上期期末在制数量");
        assert_eq!(bal.beg_period.as_deref(), Some(f.periods[0].as_str()));
        assert_eq!(bal.beg_total.as_deref().map(num), Some(dec("750")));
        assert_eq!(bal.cur_total.as_deref().map(num), Some(dec("2450")));
        assert_eq!(bal.finished_total.as_deref().map(num), Some(dec("2500")));
        assert_eq!(bal.wip_total.as_deref().map(num), Some(dec("700")));
        assert_eq!(
            num(balance_element(bal, alloc::Element::Material).wip.as_deref().unwrap()),
            dec("400")
        );
        assert_eq!(
            num(balance_element(bal, alloc::Element::Labor).wip.as_deref().unwrap()),
            dec("200")
        );
        assert_eq!(
            num(balance_element(bal, alloc::Element::Overhead).wip.as_deref().unwrap()),
            dec("100")
        );
        assert_eq!(
            num(bal.finished_total.as_deref().unwrap()) + num(bal.wip_total.as_deref().unwrap()),
            num(bal.cur_total.as_deref().unwrap()) + num(bal.beg_total.as_deref().unwrap()),
            "完工 + 期末 = 期初 + 投入"
        );

        f.cleanup().await;
    }

    // ==================== 17. 改一道工序，三张表一起动 ====================

    #[tokio::test]
    async fn d3_one_operation_change_moves_all_three_tables() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();
        let object = f.object("D3", "OPEN", "800", "200", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "LABOR", "900", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "450", &f.periods[0]).await;

        let first = save_ops(&principal, object, standard_rows()).await;
        assert_eq!(first.rollup.expect("有汇总").pct, "50");
        let before = alloc_sheet(&principal, &f.periods[0], &f.tag("D3")).await;
        assert_eq!(
            alloc_row_of(&before, &f.object_no("D3")).wip_total.as_deref().map(num),
            Some(dec("350")),
            "50% 口径：在制 350"
        );

        // 只改「焊接」：50% → 100%（权重不变）⇒ (1×100 + 2×100 + 1×0) / 4 = 75%
        let second = save_ops(
            &principal,
            object,
            vec![
                op_row(1, "下料", "1", "100"),
                op_row(2, "焊接", "2", "100"),
                op_row(3, "装配", "1", "0"),
            ],
        )
        .await;
        assert!(second.changed, "改了一道工序的完工率必须算修改");
        assert_eq!(second.rollup.expect("有汇总").pct, "75");
        assert_eq!(f.object_pct(object).await, dec("75"), "对象级跟着回写 75");

        // ---- 约当：加工约当 800 + 200 × 75% = 950 ----
        let eq_sheet = equiv_sheet(&principal, &f.periods[0], &f.tag("D3")).await;
        let eq_row = equiv_row_of(&eq_sheet, &f.object_no("D3"));
        assert_eq!(num(&eq_row.completion_pct), dec("75"));
        assert_eq!(eq_row.equiv_conversion.as_deref().map(num), Some(dec("950")));
        assert_eq!(
            eq_row.equiv_material.as_deref().map(num),
            Some(dec("1000")),
            "材料约当不受完工率影响"
        );

        // ---- 分配：人工 900 ÷ 950、费用 450 ÷ 950 ----
        let alloc_sheet_v = alloc_sheet(&principal, &f.periods[0], &f.tag("D3")).await;
        let row = alloc_row_of(&alloc_sheet_v, &f.object_no("D3"));
        assert_eq!(row.equiv_conversion.as_deref().map(num), Some(dec("950")));
        assert_eq!(
            num(alloc_element(row, alloc::Element::Material).wip.as_deref().unwrap()),
            dec("200"),
            "材料侧不动"
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Labor).rate.as_deref().unwrap()),
            dec("0.947368"),
            "900 ÷ 950（6 位）"
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Labor).wip.as_deref().unwrap()),
            dec("142.1056"),
            "900 − 800 × 0.947368"
        );
        assert_eq!(
            num(alloc_element(row, alloc::Element::Overhead).wip.as_deref().unwrap()),
            dec("71.0528")
        );
        assert_eq!(row.wip_total.as_deref().map(num), Some(dec("413.1584")));
        assert_balanced(row);

        // ---- 余额：与分配同数，且大于 50% 那一次（进度更高但仍在制）----
        let bal_sheet = balance_sheet(&principal, &f.periods[0], &f.tag("D3")).await;
        let bal = balance_row_of(&bal_sheet, &f.object_no("D3"));
        assert_eq!(num(&bal.completion_pct), dec("75"));
        assert_eq!(bal.wip_total.as_deref().map(num), Some(dec("413.1584")));
        assert_eq!(
            num(balance_element(bal, alloc::Element::Labor).wip.as_deref().unwrap()),
            dec("142.1056"),
            "余额表逐要素吃分配结果"
        );
        assert_eq!(num(&bal_sheet.totals.wip_total), dec("413.1584"));
        assert!(
            num(&bal_sheet.totals.wip_total) > dec("350"),
            "完工率 75% 时加工约当变大 ⇒ 更多加工成本留在在制"
        );

        f.cleanup().await;
    }

    // ==================== 18. 算不出来：三张表都 blocked ====================

    #[tokio::test]
    async fn d4_blocked_row_is_null_in_all_three_tables() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();
        // 在制 200 但完工率 0（没有工序，接口也不许把全 0 汇总存进去 ⇒ 保持 400 的判据）
        let object = f.object("D4", "OPEN", "800", "200", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "LABOR", "900", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "450", &f.periods[0]).await;

        let eq_sheet = equiv_sheet(&principal, &f.periods[0], &f.tag("D4")).await;
        let eq_row = equiv_row_of(&eq_sheet, &f.object_no("D4"));
        assert_eq!(eq_row.state, "blocked");
        assert_eq!(eq_row.equiv_conversion, None, "加工约当算不出来 ⇒ null");
        assert_eq!(
            eq_row.equiv_material.as_deref().map(num),
            Some(dec("1000")),
            "材料侧照算"
        );
        assert!(
            eq_row.issues.iter().any(|issue| issue.code == "WIP_PCT_MISSING"),
            "问题码要点名缺完工率：{:?}",
            eq_row.issues
        );

        let alloc_sheet_v = alloc_sheet(&principal, &f.periods[0], &f.tag("D4")).await;
        let row = alloc_row_of(&alloc_sheet_v, &f.object_no("D4"));
        assert_eq!(row.state, "blocked");
        assert_eq!(row.wip_total, None, "金额一律 null（不画假 0）");
        assert_eq!(row.finished_total, None);
        for element in alloc::Element::ALL {
            assert_eq!(alloc_element(row, element).wip, None, "{} 在制 null", element.label());
            assert_eq!(
                alloc_element(row, element).finished,
                None,
                "{} 完工 null",
                element.label()
            );
        }

        let bal_sheet = balance_sheet(&principal, &f.periods[0], &f.tag("D4")).await;
        let bal = balance_row_of(&bal_sheet, &f.object_no("D4"));
        assert_eq!(bal.wip_total, None);
        assert_eq!(bal.carry.state, "blocked", "连余额都没有 ⇒ 谈不上结转");
        assert!(bal.carry.needs_attention);
        assert!(!bal.elements_available);
        assert_eq!(bal_sheet.totals.blocked_count, 1, "合计里要点出「有 1 行算不出来」");
        assert_eq!(num(&bal_sheet.totals.wip_total), Decimal::ZERO, "坏行不进金额合计");

        f.cleanup().await;
    }

    // ==================== 19. 两行合计：完工行 + 在制行 ====================

    #[tokio::test]
    async fn d5_totals_of_finished_and_wip_rows_keep_the_identity() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();

        // A 行：多工序 50%、有在制（1000 / 900 / 450）
        let a = f.object("D5A", "OPEN", "800", "200", "0").await;
        f.collect(a, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(a, "LABOR", "900", &f.periods[0]).await;
        f.collect(a, "OVERHEAD", "450", &f.periods[0]).await;
        save_ops(&principal, a, standard_rows()).await;

        // B 行：全部完工（在制 0，累计 100），投入 100 / 100 / 100
        let b = f.object("D5B", "OPEN", "100", "0", "0").await;
        f.collect(b, "MATERIAL", "100", &f.periods[0]).await;
        f.collect(b, "LABOR", "100", &f.periods[0]).await;
        f.collect(b, "OVERHEAD", "100", &f.periods[0]).await;

        // 两行的对象号是 D5A / D5B，用后缀收窄（`tag("D5")` 只匹配 D5-<后缀>，匹配不到 D5A-…）
        let tag = crate::test_seed::last_chars(&f.suffix, 8);

        let bal_sheet = balance_sheet(&principal, &f.periods[0], &tag).await;
        assert_eq!(bal_sheet.totals.row_count, 2, "两行都进表");
        assert_eq!(bal_sheet.totals.wip_row_count, 1, "只有 A 行有余额");
        assert_eq!(bal_sheet.totals.cleared_count, 1, "B 行无余额");
        assert_eq!(num(&bal_sheet.totals.cur_total), dec("2650"), "2350 + 300");
        assert_eq!(num(&bal_sheet.totals.finished_total), dec("2300"), "A 2000 + B 300");
        assert_eq!(num(&bal_sheet.totals.wip_total), dec("350"));
        assert_eq!(
            num(&bal_sheet.totals.finished_total) + num(&bal_sheet.totals.wip_total),
            num(&bal_sheet.totals.cur_total),
            "完工 + 余额 = 本期投入（首期无期初）"
        );
        assert_eq!(
            num(&bal_sheet.totals.wip_material)
                + num(&bal_sheet.totals.wip_labor)
                + num(&bal_sheet.totals.wip_overhead),
            num(&bal_sheet.totals.wip_total)
        );
        assert_eq!(bal_sheet.totals.element_row_count, 2);

        let eq_sheet = equiv_sheet(&principal, &f.periods[0], &tag).await;
        assert_eq!(eq_sheet.totals.row_count, 2);
        assert_eq!(eq_sheet.totals.wip_row_count, 1);
        assert_eq!(eq_sheet.totals.finished_row_count, 1);

        let alloc_sheet_v = alloc_sheet(&principal, &f.periods[0], &tag).await;
        assert_eq!(num(&alloc_sheet_v.totals.wip_total), num(&bal_sheet.totals.wip_total));
        assert_eq!(
            num(&alloc_sheet_v.totals.pool_total),
            num(&bal_sheet.totals.beg_total) + num(&bal_sheet.totals.cur_total)
        );

        f.cleanup().await;
    }

    // ==================== 20. 清场 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let principal = f.super_principal();
        let object = f.object("ZZ", "OPEN", "100", "20", "0").await;
        f.collect(object, "MATERIAL", "100", &f.periods[0]).await;
        save_ops(&principal, object, vec![op_row(1, "唯一工序", "1", "50")]).await;
        assert!(f.residue().await > 0, "清场前必须有行");

        f.cleanup().await;
        assert_eq!(f.residue().await, 0, "清场后本夹具零残留");
    }
}
