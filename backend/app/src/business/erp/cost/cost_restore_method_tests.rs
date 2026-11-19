//! 成本还原**全链路单测**（M5-29）：三级（本项目按**四层**验，层级 1–4）还原，
//! 把三张表串在**同一个夹具**上钉住。
//!
//! # 这张卡补的是「卡与卡之间的缝」
//!
//! M5-26（结构）/ M5-27（还原）/ M5-28（报表）各自有单测，但都是**各测各的**：
//! 结构表测「层长得对不对」，还原表测「钱拆得对不对」，报表测「分组加得对不对」。
//! 中间的缝没人钉：**同一个期间、同一个对象，三张表说的数字必须对得上**，
//! 而且层数一变（`max_depth`），三张表的「卡在第几层」必须同步。这张卡就测这条缝：
//!
//! ```text
//!   M5-26 结构表   depth 4 / layers[1..4] / 第 1 层 = 本期材料投入
//!        │  同一棵树
//!        ▼
//!   M5-27 还原表   flows[1..4] / 第 k 层流入 = 第 k-1 层拨下来的材料
//!        │  同一批行
//!        ▼
//!   M5-28 报表     group_by=object 的一组 == 明细表同一行（8 个金额字段逐字对拍）
//! ```
//!
//! # 夹具链（四层，全项目最深的还原路径）
//!
//! ```text
//!  F 批（期间 P4）：材料 1000 = S2 600 + 外购 R2 400；工 200 / 费 200   → 完工 1400
//!   S2 批（P3）：材料 S3 600；工 200 / 费 200                            → 结构 600/200/200
//!    S3 批（P2）：材料 S4 600；工 200 / 费 200                          → 结构 600/200/200
//!     S4 批（P1）：材料 外购 R 600；工 200 / 费 200                     → 结构 600/200/200
//! ```
//!
//! 每一层的结构都是 60% / 20% / 20%，所以往下拨的材料逐层 ×0.6：
//! `1000 → 600 → 360 → 216 → 129.6`，最后 129.6 落在**外购料 R** 上。
//! 于是还原后的料工费可以手算，用例里的期望值不是抄输出，是按这条链推出来的：
//!
//! | 层级 | 流入 | 外购料 | 人工 | 制造费用 | 往下拨 | 不彻底 |
//! |---|---|---|---|---|---|---|
//! | 1 | 1000 | 400（R2） | 120 | 120 | 360 | 0 |
//! | 2 | 360 | 0 | 72 | 72 | 216 | 0 |
//! | 3 | 216 | 0 | 43.2 | 43.2 | 129.6 | 0 |
//! | 4 | 129.6 | 129.6（R） | 0 | 0 | 0 | 0 |
//!
//! 成品行：还原前 1000 / 200 / 200 = 1400 → 还原后 **529.6 / 435.2 / 435.2** = 1400。
//!
//! # 三级还原怎么测（`max_depth` 1 → 4）
//!
//! `max_depth` 是「允许展开几层」。链有 4 层，于是：
//!
//! | `max_depth` | 卡在第几层 | 不彻底 | 外购料 | 人工 | 制造费用 |
//! |---|---|---|---|---|---|
//! | 1 | S2 | 360 | 400 | 320 | 320 |
//! | 2 | S3 | 216 | 400 | 392 | 392 |
//! | 3 | S4 | 129.6 | 400 | 435.2 | 435.2 |
//! | 4 | 到底 | 0 | 529.6 | 435.2 | 435.2 |
//!
//! 关键洞察（用例里逐条断言）：**截断只把「材料」挡在上一层**，已经拆出来的工 / 费一分不动 ——
//! 因为卡住的那一层自己的完工成本结构是齐的，拆得动的部分照拆。所以
//! `max_depth=3`（三级还原）与 `max_depth=4` 的差别**只有**外购料 129.6 与不彻底 129.6 互换。
//!
//! # 另一种「卡住」：结构表到底了，还原表还差一层
//!
//! `max_depth=3` 时结构表**不报截断**（`probe_depth_cut` 探查发现 S4 下面全是外购料，
//! 结构本来就到底了 ⇒ `state = semi`），但还原表仍是 `partial`：还原要把「材料部分」
//! 变成**外购料叶子**才算确认，S4 没有子层 ⇒ 那 129.6 只能记「还原不彻底」。
//! 这不是 bug，是两张表的口径差（结构问「是什么」，还原问「钱变成了什么」），用例把它钉住。
//!
//! # 跑法
//!
//! ```bash
//! ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m529_head" \
//!   cargo test --lib cost_restore_method -- --test-threads=1
//! ```
//!
//! 真库部分不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 真库用例同口径）。
//!
//! # 夹具纪律
//!
//! * 期间槽独占 `1600 + slot`（M5-26 用 `1900+slot`、M5-27 `1800+slot`、M5-28 `1700+slot`、
//!   在制/分批用 `2000+slot`），避免撞 `uk_biz_cost_period`；
//! * 对象号 / 批号 / 物料编码 / 领料单号全带 `M529` 前缀 + 唯一后缀，列表断言一律 `keyword` 收窄；
//! * 计算单用 `cost_period_item_dto::compose` 算好再落库（与真 calc 同一段代码），不手抄数字；
//! * 最后一条用例断言清场零残留。

use nik::prelude::*;

// ============================================================================
// 一、纯逻辑（不连库）：四层链 + 三级还原
// ============================================================================

#[cfg(test)]
mod pure {
    use crate::business::erp::cost::cost_restore_rollup_dto::{self as dto, ElementSplit, ElementView, NodeView};
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

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

    /// 自制半成品节点；`cut = Some(它的层级)` ⇒ 砍掉子层并标 `truncated`
    /// （与 M5-26 出参同形：到层数上限的节点保留 `elements`、`children` 为空）。
    fn semi(
        level: u32,
        code: &str,
        amount: &str,
        split: (&str, &str, &str),
        children: Vec<NodeView>,
        cut: Option<u32>,
    ) -> NodeView {
        let stopped = cut == Some(level);
        let mut item = node(
            level,
            code,
            amount,
            Some(split),
            true,
            if stopped { Vec::new() } else { children },
        );
        item.truncated = stopped;
        item
    }

    /// 四层链（与真库夹具同一组比例）：见模块头的图。
    ///
    /// `cut` = `Some(level)` 时把该层半成品标成截断 —— 用来模拟 `max_depth = level`。
    fn four_level_chain(cut: Option<u32>) -> Vec<NodeView> {
        let leaf_in_s4 = purchased(4, "R", "600");
        let s4 = semi(3, "S4", "600", ("360", "120", "120"), vec![leaf_in_s4], cut);
        let s3 = semi(2, "S3", "600", ("360", "120", "120"), vec![s4], cut);
        let s2 = semi(1, "S2", "600", ("360", "120", "120"), vec![s3], cut);
        let leaf_in_f = purchased(1, "R2", "400");
        vec![s2, leaf_in_f]
    }

    fn before(material: &str, labor: &str, overhead: &str) -> ElementSplit {
        ElementSplit {
            material: dec(material),
            labor: dec(labor),
            overhead: dec(overhead),
        }
    }

    /// 逐层台账的期望值（流入 / 外购 / 人工 / 费 / 往下拨 / 不彻底）。
    fn assert_flows(rollup: &dto::Rollup, expected: &[(&str, &str, &str, &str, &str, &str)]) {
        assert_eq!(rollup.flows.len(), expected.len(), "台账层数");
        for (index, (imputed, purchased, labor, overhead, carried, unresolved)) in expected.iter().enumerate() {
            let flow = &rollup.flows[index];
            assert_eq!(
                flow.level,
                u32::try_from(index + 1).unwrap_or(0),
                "第 {} 层序号",
                index + 1
            );
            assert_eq!(flow.imputed, dec(imputed), "第 {} 层流入", index + 1);
            assert_eq!(flow.purchased, dec(purchased), "第 {} 层外购料", index + 1);
            assert_eq!(flow.labor, dec(labor), "第 {} 层人工", index + 1);
            assert_eq!(flow.overhead, dec(overhead), "第 {} 层制造费用", index + 1);
            assert_eq!(flow.carried_down, dec(carried), "第 {} 层往下拨", index + 1);
            assert_eq!(flow.unresolved, dec(unresolved), "第 {} 层不彻底", index + 1);
            assert!(
                flow.balanced(),
                "第 {} 层台账不自洽：流入 ≠ 外购 + 工 + 费 + 往下拨 + 不彻底",
                index + 1
            );
        }
    }

    // ---------- 用例 ----------

    /// ① 四层链一路卷积到外购料：529.6 / 435.2 / 435.2，四层台账逐层自洽。
    #[test]
    fn four_levels_are_convolved_to_purchased_material() {
        let rollup = dto::restore_of(&four_level_chain(None), before("1000", "200", "200"));

        assert_eq!(
            rollup.after,
            ElementSplit {
                material: dec("529.6"),
                labor: dec("435.2"),
                overhead: dec("435.2"),
            },
            "外购料 400（R2）+ 129.6（R），人工/费各 200（本期）+ 120 + 72 + 43.2（从材料里拆出来的）"
        );
        assert_eq!(rollup.unresolved, Decimal::ZERO, "链钻到底了，没有不彻底");
        assert_eq!(rollup.after.total(), dec("1400"), "还原不动总额");
        assert_eq!(rollup.covered, dec("1000"), "第 1 层覆盖的材料投入");
        assert!(rollup.balanced());
        assert!(!rollup.truncated);
        assert!(!rollup.missing);

        assert_flows(
            &rollup,
            &[
                ("1000", "400", "120", "120", "360", "0"),
                ("360", "0", "72", "72", "216", "0"),
                ("216", "0", "43.2", "43.2", "129.6", "0"),
                ("129.6", "129.6", "0", "0", "0", "0"),
            ],
        );

        // 层与层是「往下拨」：第 k 层的流入 = 第 k-1 层拨下来的金额（不是新的投入，别相加）
        for pair in rollup.flows.windows(2) {
            assert_eq!(
                pair[1].imputed, pair[0].carried_down,
                "第 {} 层的流入必须等于第 {} 层拨下来的金额",
                pair[1].level, pair[0].level
            );
        }
        // 外购料合计 = 各层外购料之和（每层只确认自己那一段）
        let purchased: Decimal = rollup.flows.iter().map(|flow| flow.purchased).sum();
        assert_eq!(purchased, rollup.after.material, "还原后材料 = 各层确认的外购料之和");
    }

    /// ② 三级还原（`max_depth = 3`）：停在第 3 层，129.6 记不彻底；已拆出的工 / 费一分不动。
    #[test]
    fn three_level_cut_stops_after_the_third_level() {
        let full = dto::restore_of(&four_level_chain(None), before("1000", "200", "200"));
        let three = dto::restore_of(&four_level_chain(Some(3)), before("1000", "200", "200"));

        assert_eq!(three.unresolved, dec("129.6"), "第 3 层那笔材料的 60% 还是半成品形态");
        assert_eq!(three.after.material, dec("400"), "只有第 1 层的外购料 R2 被确认");
        assert_eq!(three.after.labor, dec("435.2"));
        assert_eq!(three.after.overhead, dec("435.2"));
        assert_eq!(three.after.total() + three.unresolved, dec("1400"), "守恒");
        assert!(three.balanced());
        assert!(three.truncated, "到层数上限必须如实标出来");

        // 三级 vs 四级：唯一的差别是第 4 层的 129.6 从「外购料」挪到「不彻底」
        assert_eq!(
            full.after.material - three.after.material,
            dec("129.6"),
            "多还原一层只多确认 129.6 外购料"
        );
        assert_eq!(three.unresolved, full.after.material - three.after.material);
        assert_eq!(full.after.labor, three.after.labor, "截断不动已经拆出来的工 / 费");
        assert_eq!(full.after.overhead, three.after.overhead);
        assert_eq!(
            three.after.total() + three.unresolved,
            full.after.total(),
            "三要素合计少的正好是不彻底那笔（还原后合计 + 不彻底 = 还原前合计）"
        );

        assert_flows(
            &three,
            &[
                ("1000", "400", "120", "120", "360", "0"),
                ("360", "0", "72", "72", "216", "0"),
                ("216", "0", "43.2", "43.2", "0", "129.6"),
            ],
        );

        // 半成品明细到第 3 层为止，且最后一层标「停在这里」
        assert_eq!(three.semis.len(), 3);
        assert_eq!(
            three.semis.iter().map(|item| item.level).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(!three.semis[0].stopped);
        assert!(!three.semis[1].stopped);
        assert!(three.semis[2].stopped, "S4 停在第 3 层");
        assert_eq!(three.semis[2].restored.unresolved, dec("129.6"));
    }

    /// ③ 一级 / 二级 / 三级 / 到底：四个档位的金额逐档对上（这就是 `max_depth` 1–4）。
    #[test]
    fn every_cut_depth_moves_exactly_the_material_part_into_unresolved() {
        let expected = [
            // (max_depth, 不彻底, 外购料, 人工, 制造费用)
            (1_u32, "360", "400", "320", "320"),
            (2, "216", "400", "392", "392"),
            (3, "129.6", "400", "435.2", "435.2"),
            (4, "0", "529.6", "435.2", "435.2"),
        ];
        for (depth, unresolved, material, labor, overhead) in expected {
            let cut = if depth < 4 { Some(depth) } else { None };
            let rollup = dto::restore_of(&four_level_chain(cut), before("1000", "200", "200"));
            assert_eq!(rollup.unresolved, dec(unresolved), "max_depth={depth} 的不彻底");
            assert_eq!(rollup.after.material, dec(material), "max_depth={depth} 的外购料");
            assert_eq!(rollup.after.labor, dec(labor), "max_depth={depth} 的人工");
            assert_eq!(rollup.after.overhead, dec(overhead), "max_depth={depth} 的制造费用");
            assert_eq!(
                rollup.after.total() + rollup.unresolved,
                dec("1400"),
                "max_depth={depth} 必须守恒"
            );
            assert_eq!(
                rollup.flows.len(),
                usize::try_from(depth).unwrap_or(0),
                "max_depth={depth} 的台账层数"
            );
            // 不彻底只增不减：层数越少，停在半成品形态的钱越多
            assert!(rollup.unresolved <= dec(unresolved));
        }
    }

    /// ④ 半成品明细与台账对得上：每层半成品的「被卷积金额」= 该层台账流入里属于半成品的那部分。
    #[test]
    fn semi_details_carry_the_imputed_amount_of_their_own_level() {
        let rollup = dto::restore_of(&four_level_chain(None), before("1000", "200", "200"));
        assert_eq!(rollup.semis.len(), 3);

        // 第 1 层：S2 拿走 600（R2 那 400 是外购料，不进半成品明细）
        assert_eq!(rollup.semis[0].level, 1);
        assert_eq!(rollup.semis[0].imputed, dec("600"));
        assert_eq!(
            rollup.semis[0].imputed + rollup.flows[0].purchased,
            rollup.flows[0].imputed,
            "第 1 层流入 = 半成品 + 外购料"
        );
        // 第 2 层起：本层流入全部来自上一层半成品的材料部分
        for (index, detail) in rollup.semis.iter().enumerate().skip(1) {
            assert_eq!(detail.level, u32::try_from(index + 1).unwrap_or(0));
            assert_eq!(
                detail.imputed, rollup.flows[index].imputed,
                "第 {} 层流入 = 该层半成品的被卷积金额",
                detail.level
            );
        }
        // 每条明细：还原后 + 不彻底 = 被卷积金额（半成品自己那笔钱不能凭空多 / 少）
        for detail in &rollup.semis {
            assert_eq!(
                detail.restored.total() + detail.restored.unresolved,
                detail.imputed,
                "第 {} 层半成品 {} 的明细不自洽",
                detail.level,
                detail.material_code.clone().unwrap_or_default()
            );
        }
        // 三层明细的「材料」都指向同一笔最终外购料：129.6（越往上越含工费）
        assert_eq!(rollup.semis[0].restored.material, dec("129.6"));
        assert_eq!(rollup.semis[1].restored.material, dec("129.6"));
        assert_eq!(rollup.semis[2].restored.material, dec("129.6"));
        assert_eq!(rollup.semis[0].restored.labor, dec("235.2"), "120 + 72 + 43.2");
        assert_eq!(rollup.semis[1].restored.labor, dec("115.2"), "72 + 43.2");
        assert_eq!(rollup.semis[2].restored.labor, dec("43.2"));
    }

    /// ⑤ 分叉 BOM：第 1 层两个半成品各自下钻，兄弟之间按金额分配、合起来守恒。
    #[test]
    fn branching_bom_allocates_by_sibling_amount_and_conserves() {
        // A 600（结构 60/20/20）→ 外购料 600；B 400（结构 50/25/25）→ 半成品 T 200 → 外购料 200
        let leaf_a = purchased(2, "RA", "600");
        let a = semi(1, "A", "600", ("360", "120", "120"), vec![leaf_a], None);
        let leaf_b = purchased(3, "RB", "400");
        let t = semi(2, "T", "400", ("200", "100", "100"), vec![leaf_b], None);
        let b = semi(1, "B", "400", ("200", "100", "100"), vec![t], None);
        let rollup = dto::restore_of(&[a, b], before("1000", "0", "0"));

        // A：拆 360/120/120，材料 360 一路确认成外购料
        // B：拆 200/100/100，材料 200 再拆成 T 的 100/50/50，T 的材料 100 确认成外购料
        assert_eq!(rollup.after.material, dec("460"), "360 + 100");
        assert_eq!(rollup.after.labor, dec("270"), "120 + 100 + 50");
        assert_eq!(rollup.after.overhead, dec("270"), "120 + 100 + 50");
        assert_eq!(rollup.unresolved, Decimal::ZERO);
        assert!(rollup.balanced());

        assert_flows(
            &rollup,
            &[
                // 第 1 层：A 600 + B 400（各自拆出 120/120 与 100/100）
                ("1000", "0", "220", "220", "560", "0"),
                // 第 2 层：A 的材料 360 全是外购料；B 的材料 200 落在半成品 T 上，按 T 的结构再拆 100/50/50
                ("560", "360", "50", "50", "100", "0"),
                // 第 3 层：T 的材料 100 全是外购料
                ("100", "100", "0", "0", "0", "0"),
            ],
        );
        assert_eq!(rollup.semis.len(), 3, "A、B、T 三条半成品明细");
        assert_eq!(rollup.semis[0].imputed, dec("600"), "A");
        assert_eq!(rollup.semis[1].imputed, dec("400"), "B");
        assert_eq!(rollup.semis[2].imputed, dec("200"), "T（B 拨下来的材料部分）");
    }

    /// ⑥ 任何截断深度都守恒，且台账每层自洽（不允许「层数一变钱就多 / 少」）。
    #[test]
    fn every_depth_keeps_the_before_total() {
        let before = before("1000", "200", "200");
        for cut in [Some(1), Some(2), Some(3), None] {
            let rollup = dto::restore_of(&four_level_chain(cut), before);
            assert_eq!(
                rollup.after.total() + rollup.unresolved,
                before.total(),
                "cut={cut:?} 守恒失败"
            );
            assert!(rollup.balanced(), "cut={cut:?}");
            for flow in &rollup.flows {
                assert!(flow.balanced(), "cut={cut:?} 第 {} 层台账不自洽", flow.level);
            }
        }
    }
}

// ============================================================================
// 二、真库全链路：一个夹具，三张表，同步变化
// ============================================================================

#[cfg(test)]
mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{self as item_dto, BegCost, ItemInput, OutputInput};
    use crate::business::erp::cost::cost_restore_report_dto::{GroupRow, ReportFilter, ReportQuery, ReportRes};
    use crate::business::erp::cost::cost_restore_report_handler::{filter_of as report_filter_of, report_of};
    use crate::business::erp::cost::cost_restore_rollup_dto::{RollupFilter, RollupQuery, RollupRes, Row as RollupRow};
    use crate::business::erp::cost::cost_restore_rollup_handler::{filter_of as rollup_filter_of, rollup_table_of};
    use crate::business::erp::cost::cost_restore_structure_dto::{
        NodeView, RestoreQuery, RestoreRes, Row as StructureRow,
    };
    use crate::business::erp::cost::cost_restore_structure_handler::{
        filter_of as structure_filter_of, structure_table_of,
    };
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
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字面量：{text}"))
    }

    /// 出参里的金额是字符串：按 Decimal 比，避免「1000 vs 1000.0000」这种标度假红。
    fn num(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字符串无法解析：{text}"))
    }

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    struct Fixture {
        suffix: String,
        /// 四个连续期间 01 / 02 / 03 / 04（四级半成品链 → 成品）。
        periods: [String; 4],
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        unit: Uuid,
        warehouse: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            // 1600+ 让开 M5-26（1900+）/ M5-27（1800+）/ M5-28（1700+）/ 在制分批（2000+）
            let year = 1600 + (slot % 80) as i32;
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
                format!("{year:04}-04"),
            ];
            Self::reset().await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &format!("M529-DEPT2-{suffix}"),
                &format!("M5-29 夹具第二车间 {suffix}"),
            )
            .await;
            let unit = crate::test_seed::ensure_unit().await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M529W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-29 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
                unit,
                warehouse,
            }
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        fn other_dept_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(self.other_dept),
                false,
                ScopeKind::Dept,
                vec![self.other_dept],
                false,
            )
        }

        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M529-{}", self.tag(name))
        }

        fn lot(&self, name: &str) -> String {
            format!("LOT-M529-{}", self.tag(name))
        }

        /// 本夹具的唯一标记（8 位）：`keyword` 用它把三张表同时收窄到**本夹具**的对象，
        /// 不会被同库里别人跑过的对象串进来（对象号 / 批号 / 物料编码都带这 8 位）。
        fn mark(&self) -> String {
            crate::test_seed::last_chars(&self.suffix, 8)
        }

        /// 造物料（`RAW` / `SEMI` / `FINISHED`）。
        async fn material(&self, kind: &str, name: &str) -> Uuid {
            crate::test_seed::ensure_material_kind(
                &format!("M529-{kind}-{}-{name}", crate::test_seed::last_chars(&self.suffix, 6)),
                &format!("M5-29 夹具 {kind} {name} {}", self.suffix),
                kind,
            )
            .await
        }

        /// 造成本对象（数量 / 在制 / 完工率同时写到对象上，与计算单一致）。
        async fn object(&self, name: &str, product: Uuid, lot: &str, completed: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  completion_pct, material_input_mode, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 1000, ?::text::numeric, \
                             0, 0, 'ONE_TIME', ?::uuid, ?, '{}-01', 'OPEN', now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&product),
                    Value::Text(lot.to_string()),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 造一条**带批号**的领料行（先造一张已过账领料单明细，再让归集行指向它）。
        async fn collect_material_with_lot(
            &self,
            object: Uuid,
            period: &str,
            material: Uuid,
            lot: &str,
            qty: &str,
            amount: &str,
        ) -> Uuid {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let out_no = format!("OUT-M529-{}-{seq}", crate::test_seed::last_chars(&self.suffix, 6));
            let stock_out = Uuid::now_v7();
            let item = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, 'POSTED', \
                         ?::text::numeric, ?::text::numeric, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&stock_out),
                    Value::Text(out_no.clone()),
                    Value::from(&self.warehouse),
                    Value::Text(format!("{period}-10")),
                    Value::Text(qty.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造领料单夹具");
            db().exec_sql(
                "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, \
                                                  unit_cost, amount, lot_no, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&item),
                    Value::from(&stock_out),
                    Value::from(&material),
                    Value::from(&self.unit),
                    Value::Text(qty.to_string()),
                    Value::Text((dec(amount) / dec(qty)).round_dp(6).to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(lot.to_string()),
                ],
            )
            .await
            .expect("造领料明细夹具");
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                source_id, source_no, line_no, occur_date, material_id, \
                                                qty, unit_price, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, ?, 1, ?::text::date, \
                         ?::uuid, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         'M5-29 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::from(&item),
                    Value::Text(out_no),
                    Value::Text(format!("{period}-10")),
                    Value::from(&material),
                    Value::Text(qty.to_string()),
                    Value::Text((dec(amount) / dec(qty)).round_dp(6).to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造带批号归集行夹具");
            stock_out
        }

        /// 造一条工 / 费归集行（不许挂物料）。
        async fn collect_element(&self, object: Uuid, period: &str, element: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                line_no, occur_date, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, ?::text::numeric, \
                         'M5-29 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::Text(element.to_string()),
                    Value::Text(format!("{period}-10")),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造工 / 费归集行夹具");
        }

        /// 造成本核算批次。
        async fn batch(&self, period: &str, status: &str, tag: &str) -> Uuid {
            let id = Uuid::now_v7();
            let needs_approval = matches!(status, "SETTLED" | "REVERSED" | "CLOSED");
            let settled = if needs_approval { "now()" } else { "NULL" };
            let approval = if needs_approval { "?::uuid" } else { "NULL" };
            let voucher = if needs_approval { "?::uuid" } else { "NULL" };
            let reversed_at = if status == "REVERSED" { "now()" } else { "NULL" };
            let reason = if status == "REVERSED" { "?::text" } else { "NULL" };
            let mut params = vec![
                Value::from(&id),
                Value::Text(period.to_string()),
                Value::Text(format!(
                    "CS-M529-{tag}-{}",
                    crate::test_seed::last_chars(&self.suffix, 8)
                )),
                Value::Text(status.to_string()),
            ];
            if needs_approval {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            if status == "REVERSED" {
                params.push(Value::Text("M5-29 夹具红冲".to_string()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period (id, period_code, batch_no, status, \
                                                  approval_instance_id, settled_at, finish_voucher_id, \
                                                  reversed_at, reverse_reason, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?, {approval}, {settled}, {voucher}, {reversed_at}, {reason}, now(), now())"
                ),
                params,
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        /// 用 **真 calc 的 `compose`** 算一张计算单再落库（不是手抄数字）。
        #[allow(clippy::too_many_arguments)]
        async fn item(
            &self,
            object: Uuid,
            object_name: &str,
            batch: Uuid,
            product: Uuid,
            period: &str,
            completed: &str,
            mat: &str,
            labor: &str,
            oh: &str,
        ) {
            let input = ItemInput {
                period_code: period.to_string(),
                cost_object_id: object,
                object_no: self.object_no(object_name),
                product_id: product,
                output: OutputInput {
                    cumulative_completed_qty: dec(completed),
                    prior_completed_qty: Decimal::ZERO,
                    wip_end_qty: Decimal::ZERO,
                    completion_pct: Decimal::ZERO,
                    prior_wip_qty: Decimal::ZERO,
                    material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
                },
                beg: BegCost::default(),
                mat_cur: dec(mat),
                labor_cur: dec(labor),
                oh_cur: dec(oh),
            };
            let calc = item_dto::compose(&input).expect("夹具计算单必须可算（compose 拒绝了输入）");
            db().exec_sql(
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
                         ?::text::numeric, ?::text::numeric, 'CALC', NULL, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&batch),
                    Value::from(&object),
                    Value::from(&product),
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
                ],
            )
            .await
            .expect("造计算单夹具");
        }

        /// 清掉本卡之前跑过的残留（只清本卡能造的形态，按前缀 / 批号）。
        async fn reset() {
            for statement in [
                "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M529-%')",
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-29 夹具'",
                "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-M529-%'",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M529-%'",
                "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M529-%'",
            ] {
                let _ = db().exec_sql(statement.to_string(), vec![]).await;
            }
        }

        /// 清场（FK 顺序：计算单 → 归集）。
        async fn cleanup(&self, objects: &[Uuid]) {
            if objects.is_empty() {
                return;
            }
            let list = objects
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            for statement in [
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
            ] {
                db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
            }
        }

        /// 本夹具残留行数（清场断言用：按对象号 / 批号前缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE 'CB-M529-%') + \
                        (SELECT count(*) FROM biz_cost_period WHERE batch_no LIKE 'CS-M529-%') + \
                        (SELECT count(*) FROM biz_cost_collect WHERE remark = 'M5-29 夹具')",
                vec![],
            )
            .await
            .expect("残留统计")
        }
    }

    // ---------- 调用入口：三张表各一个（同一个夹具，只差调用哪个接口） ----------

    async fn structure(principal: &ScopedPrincipal, period: &str, keyword: &str, max_depth: Option<u32>) -> RestoreRes {
        let filter = structure_filter_of(&RestoreQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
            max_depth,
        })
        .expect("结构表筛选参数合法");
        structure_table_of(principal, &filter).await.expect("结构表查询")
    }

    async fn rollup(principal: &ScopedPrincipal, period: &str, keyword: &str, max_depth: Option<u32>) -> RollupRes {
        let filter: RollupFilter = rollup_filter_of(&RollupQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
            max_depth,
        })
        .expect("还原表筛选参数合法");
        rollup_table_of(principal, &filter).await.expect("还原表查询")
    }

    async fn report(
        principal: &ScopedPrincipal,
        period: &str,
        keyword: &str,
        group_by: &str,
        state: &str,
        max_depth: Option<u32>,
    ) -> ReportRes {
        let filter: ReportFilter = report_filter_of(&ReportQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            group_by: Some(group_by.to_string()),
            state: Some(state.to_string()),
            page: None,
            page_size: None,
            max_depth,
        })
        .expect("报表筛选参数合法");
        report_of(principal, &filter).await.expect("报表查询")
    }

    fn structure_row<'a>(res: &'a RestoreRes, object_no: &str) -> &'a StructureRow {
        res.list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| {
                panic!(
                    "结构表里没有这个对象：{object_no}；实际 = {:?}",
                    res.list.iter().map(|row| &row.object_no).collect::<Vec<_>>()
                )
            })
    }

    fn rollup_row<'a>(res: &'a RollupRes, object_no: &str) -> &'a RollupRow {
        res.list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| {
                panic!(
                    "还原表里没有这个对象：{object_no}；实际 = {:?}",
                    res.list.iter().map(|row| &row.object_no).collect::<Vec<_>>()
                )
            })
    }

    fn group_of<'a>(res: &'a ReportRes, key: &str) -> &'a GroupRow {
        res.groups.iter().find(|group| group.key == key).unwrap_or_else(|| {
            panic!(
                "报表里没有这个分组：{key}；实际 = {:?}",
                res.groups.iter().map(|group| &group.key).collect::<Vec<_>>()
            )
        })
    }

    fn find_node<'a>(nodes: &'a [NodeView], code: &str) -> &'a NodeView {
        nodes
            .iter()
            .find(|node| node.material_code.as_deref() == Some(code))
            .unwrap_or_else(|| {
                panic!(
                    "这一层没有该物料：{code}；实际 = {:?}",
                    nodes
                        .iter()
                        .filter_map(|node| node.material_code.clone())
                        .collect::<Vec<_>>()
                )
            })
    }

    fn material_code(fixture: &Fixture, kind: &str, name: &str) -> String {
        format!(
            "M529-{kind}-{}-{name}",
            crate::test_seed::last_chars(&fixture.suffix, 6)
        )
    }

    /// 四层链夹具（见模块头的图）+ 一个全外购料的对照对象（用于「三档状态」与「合计 = 各行之和」）。
    ///
    /// ```text
    /// P1 S4 批：外购 R 600（60 件 × 10）+ 工 200 + 费 200 = 1000 → 结构 600/200/200
    /// P2 S3 批：S4 600（60 件）+ 工 200 + 费 200 = 1000            → 结构 600/200/200
    /// P3 S2 批：S3 600（60 件）+ 工 200 + 费 200 = 1000            → 结构 600/200/200
    /// P4 F  批：S2 600（60 件）+ 外购 R2 400（100 件 × 4）+ 工 200 + 费 200 = 1400
    /// P4 G  批：外购 R 500（100 件 × 5）+ 工 100 + 费 100 = 700（全外购料，不参与还原）
    /// ```
    struct Chain {
        fixture: Fixture,
        raw: String,
        raw2: String,
        semi_s2: String,
        semi_s3: String,
        semi_s4: String,
        finished_f: String,
        finished_g: String,
        object_f: Uuid,
        object_g: Uuid,
        objects: Vec<Uuid>,
    }

    async fn build_chain() -> Chain {
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R").await;
        let raw2 = fixture.material("RAW", "R2").await;
        let semi_s4 = fixture.material("SEMI", "S4").await;
        let semi_s3 = fixture.material("SEMI", "S3").await;
        let semi_s2 = fixture.material("SEMI", "S2").await;
        let finished_f = fixture.material("FINISHED", "F").await;
        let finished_g = fixture.material("FINISHED", "G").await;

        let batch_1 = fixture.batch(&fixture.periods[0], "DRAFT", "P1").await;
        let batch_2 = fixture.batch(&fixture.periods[1], "DRAFT", "P2").await;
        let batch_3 = fixture.batch(&fixture.periods[2], "DRAFT", "P3").await;
        let batch_4 = fixture.batch(&fixture.periods[3], "DRAFT", "P4").await;

        let lot_s4 = fixture.lot("S4");
        let lot_s3 = fixture.lot("S3");
        let lot_s2 = fixture.lot("S2");
        let object_s4 = fixture.object("S4", semi_s4, &lot_s4, "100").await;
        let object_s3 = fixture.object("S3", semi_s3, &lot_s3, "100").await;
        let object_s2 = fixture.object("S2", semi_s2, &lot_s2, "100").await;
        let object_f = fixture.object("F", finished_f, &fixture.lot("F"), "100").await;
        let object_g = fixture.object("G", finished_g, &fixture.lot("G"), "100").await;

        // P1：S4 = 外购 R 600 + 工 200 + 费 200
        fixture
            .collect_material_with_lot(object_s4, &fixture.periods[0], raw, &fixture.lot("R1"), "100", "600")
            .await;
        fixture
            .collect_element(object_s4, &fixture.periods[0], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_s4, &fixture.periods[0], "OVERHEAD", "200")
            .await;
        fixture
            .item(
                object_s4,
                "S4",
                batch_1,
                semi_s4,
                &fixture.periods[0],
                "100",
                "600",
                "200",
                "200",
            )
            .await;

        // P2：S3 = S4 600 + 工 200 + 费 200
        fixture
            .collect_material_with_lot(object_s3, &fixture.periods[1], semi_s4, &lot_s4, "60", "600")
            .await;
        fixture
            .collect_element(object_s3, &fixture.periods[1], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_s3, &fixture.periods[1], "OVERHEAD", "200")
            .await;
        fixture
            .item(
                object_s3,
                "S3",
                batch_2,
                semi_s3,
                &fixture.periods[1],
                "100",
                "600",
                "200",
                "200",
            )
            .await;

        // P3：S2 = S3 600 + 工 200 + 费 200
        fixture
            .collect_material_with_lot(object_s2, &fixture.periods[2], semi_s3, &lot_s3, "60", "600")
            .await;
        fixture
            .collect_element(object_s2, &fixture.periods[2], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_s2, &fixture.periods[2], "OVERHEAD", "200")
            .await;
        fixture
            .item(
                object_s2,
                "S2",
                batch_3,
                semi_s2,
                &fixture.periods[2],
                "100",
                "600",
                "200",
                "200",
            )
            .await;

        // P4：F = S2 600 + 外购 R2 400 + 工 200 + 费 200
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[3], semi_s2, &lot_s2, "60", "600")
            .await;
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[3], raw2, &fixture.lot("R2"), "100", "400")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[3], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[3], "OVERHEAD", "200")
            .await;
        fixture
            .item(
                object_f,
                "F",
                batch_4,
                finished_f,
                &fixture.periods[3],
                "100",
                "1000",
                "200",
                "200",
            )
            .await;

        // P4：G = 外购 R 500 + 工 100 + 费 100（全外购料，`no_semi` 对照）
        fixture
            .collect_material_with_lot(object_g, &fixture.periods[3], raw, &fixture.lot("RG"), "100", "500")
            .await;
        fixture
            .collect_element(object_g, &fixture.periods[3], "LABOR", "100")
            .await;
        fixture
            .collect_element(object_g, &fixture.periods[3], "OVERHEAD", "100")
            .await;
        fixture
            .item(
                object_g,
                "G",
                batch_4,
                finished_g,
                &fixture.periods[3],
                "100",
                "500",
                "100",
                "100",
            )
            .await;

        Chain {
            raw: material_code(&fixture, "RAW", "R"),
            raw2: material_code(&fixture, "RAW", "R2"),
            semi_s2: material_code(&fixture, "SEMI", "S2"),
            semi_s3: material_code(&fixture, "SEMI", "S3"),
            semi_s4: material_code(&fixture, "SEMI", "S4"),
            finished_f: material_code(&fixture, "FINISHED", "F"),
            finished_g: material_code(&fixture, "FINISHED", "G"),
            object_f,
            object_g,
            objects: vec![object_s4, object_s3, object_s2, object_f, object_g],
            fixture,
        }
    }

    // ==================== 用例 ====================

    /// ① 四层链：三张表在同一个对象上对得上（层数 / 台账 / 金额 / 占比 / 守恒）。
    #[tokio::test]
    async fn e1_four_level_chain_agrees_across_the_three_tables() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let keyword = fixture.tag("F");
        let period = &fixture.periods[3];

        // ---------- M5-26 结构表 ----------
        let structure = structure(&fixture.super_principal(), period, &keyword, None).await;
        assert_eq!(structure.total, 1, "关键字收窄到本夹具的成品对象");
        let s_row = structure_row(&structure, &fixture.object_no("F"));
        assert_eq!(
            s_row.product_code.as_deref(),
            Some(chain.finished_f.as_str()),
            "行头必须指向成品 F（三张表按同一对象对齐才有意义）"
        );
        assert_eq!(num(&s_row.material_cost), dec("1000"), "本期材料投入 = S2 600 + R2 400");
        assert_eq!(num(&s_row.semi_cost), dec("600"));
        assert_eq!(num(&s_row.purchased_cost), dec("400"));
        assert_eq!(s_row.semi_share.as_deref(), Some("60.00"));
        assert_eq!(s_row.depth, 4, "四层链");
        assert_eq!(s_row.layers.len(), 4);

        // 逐层金额：第 1 层 = 材料投入 1000，第 2 层起是半成品批自己消耗的 600
        let layer_amounts: Vec<Decimal> = s_row.layers.iter().map(|layer| num(&layer.amount)).collect();
        assert_eq!(
            layer_amounts,
            vec![dec("1000"), dec("600"), dec("600"), dec("600")],
            "结构表逐层金额"
        );
        // 结构树：S2 → S3 → S4 → 外购 R，每层都是 60/20/20
        let level1 = find_node(&s_row.tree, &chain.semi_s2);
        assert_eq!(num(&level1.amount), dec("600"));
        assert_eq!(level1.source.as_deref(), Some("batch"), "批号对得上 ⇒ 按批取结构");
        let level2 = find_node(&level1.children, &chain.semi_s3);
        let level3 = find_node(&level2.children, &chain.semi_s4);
        let level4 = find_node(&level3.children, &chain.raw);
        assert!(!level4.is_semi, "第 4 层就是外购料，链到底");
        for leaf in [level1, level2, level3] {
            let elements = leaf.elements.as_ref().expect("半成品节点必须带结构");
            assert_eq!(num(&elements.material), dec("360"));
            assert_eq!(num(&elements.labor), dec("120"));
            assert_eq!(num(&elements.overhead), dec("120"));
            assert_eq!(num(&elements.total), num(&leaf.amount));
        }
        assert_eq!(find_node(&s_row.tree, &chain.raw2).amount, "400", "同层的外购料 R2");

        // ---------- M5-27 还原表 ----------
        let roll = rollup(&fixture.super_principal(), period, &keyword, None).await;
        assert_eq!(roll.total, 1);
        let r_row = rollup_row(&roll, &fixture.object_no("F"));
        assert_eq!(num(&r_row.before.material), dec("1000"));
        assert_eq!(num(&r_row.before.labor), dec("200"));
        assert_eq!(num(&r_row.before.overhead), dec("200"));
        assert_eq!(num(&r_row.before.total), dec("1400"));
        assert_eq!(num(&r_row.after.material), dec("529.6"), "400（R2）+ 129.6（R）");
        assert_eq!(num(&r_row.after.labor), dec("435.2"), "200 + 120 + 72 + 43.2");
        assert_eq!(num(&r_row.after.overhead), dec("435.2"));
        assert_eq!(num(&r_row.after.total), dec("1400"), "还原不动总额");
        assert_eq!(num(&r_row.unresolved), Decimal::ZERO);
        assert_eq!(r_row.state, "restored");
        assert_eq!(r_row.depth, 4, "还原表的层数 = 结构表的层数");
        assert_eq!(r_row.flows.len(), 4);
        assert_eq!(r_row.semis.len(), 3);

        // 逐层台账（见模块头的表）
        let expected = [
            ("1000", "400", "120", "120", "360", "0"),
            ("360", "0", "72", "72", "216", "0"),
            ("216", "0", "43.2", "43.2", "129.6", "0"),
            ("129.6", "129.6", "0", "0", "0", "0"),
        ];
        for (index, (imputed, purchased, labor, overhead, carried, unresolved)) in expected.iter().enumerate() {
            let flow = &r_row.flows[index];
            assert_eq!(flow.level, u32::try_from(index + 1).unwrap_or(0));
            assert_eq!(num(&flow.imputed), dec(imputed), "第 {} 层流入", index + 1);
            assert_eq!(num(&flow.purchased), dec(purchased), "第 {} 层外购料", index + 1);
            assert_eq!(num(&flow.labor), dec(labor), "第 {} 层人工", index + 1);
            assert_eq!(num(&flow.overhead), dec(overhead), "第 {} 层制造费用", index + 1);
            assert_eq!(num(&flow.carried_down), dec(carried), "第 {} 层往下拨", index + 1);
            assert_eq!(num(&flow.unresolved), dec(unresolved), "第 {} 层不彻底", index + 1);
        }

        // 跨表对拍：结构表第 1 层金额 = 还原表第 1 层流入 = 还原前材料
        assert_eq!(num(&s_row.layers[0].amount), num(&r_row.flows[0].imputed));
        assert_eq!(num(&r_row.flows[0].imputed), num(&r_row.before.material));
        // 更深的层：还原表每层流入 = 上一层拨下来的（结构层金额是节点自身金额，≥ 拨下来的部分）
        for index in 1..r_row.flows.len() {
            assert_eq!(
                num(&r_row.flows[index].imputed),
                num(&r_row.flows[index - 1].carried_down),
                "第 {} 层流入 = 第 {index} 层拨下来的",
                index + 1
            );
            assert!(
                num(&s_row.layers[index].amount) >= num(&r_row.flows[index].imputed),
                "第 {} 层：结构金额 {} 不能小于被卷积的 {}",
                index + 1,
                s_row.layers[index].amount,
                r_row.flows[index].imputed
            );
        }
        // 半成品明细与结构树同源：S2 600 / S3 360 / S4 216
        let semi_codes: Vec<Option<&str>> = r_row.semis.iter().map(|semi| semi.material_code.as_deref()).collect();
        assert_eq!(
            semi_codes,
            vec![
                Some(chain.semi_s2.as_str()),
                Some(chain.semi_s3.as_str()),
                Some(chain.semi_s4.as_str())
            ]
        );
        assert_eq!(num(&r_row.semis[0].imputed), dec("600"));
        assert_eq!(num(&r_row.semis[1].imputed), dec("360"));
        assert_eq!(num(&r_row.semis[2].imputed), dec("216"));
        assert_eq!(
            num(&r_row.semis[0].elements.material),
            dec("129.6"),
            "S2 里最终变成外购料的钱"
        );
        assert_eq!(num(&r_row.semis[0].elements.labor), dec("235.2"));
        assert_eq!(num(&r_row.semis[2].elements.labor), dec("43.2"));
        assert!(r_row.semis.iter().all(|semi| !semi.stopped), "链钻到底了");

        // ---------- M5-28 报表 ----------
        let rep = report(&fixture.super_principal(), period, &keyword, "object", "all", None).await;
        assert_eq!(rep.total, 1);
        let group = group_of(&rep, &chain.object_f.to_string());
        // 报表一组 = 明细表同一行（8 个金额字段逐字对拍）
        assert_eq!(group.object_count, 1);
        assert_eq!(num(&group.before.material), num(&r_row.before.material));
        assert_eq!(num(&group.before.labor), num(&r_row.before.labor));
        assert_eq!(num(&group.before.overhead), num(&r_row.before.overhead));
        assert_eq!(num(&group.before.total), num(&r_row.before.total));
        assert_eq!(num(&group.restored.material), num(&r_row.after.material));
        assert_eq!(num(&group.restored.labor), num(&r_row.after.labor));
        assert_eq!(num(&group.restored.overhead), num(&r_row.after.overhead));
        assert_eq!(num(&group.restored.unresolved), num(&r_row.unresolved));

        // 占比：分母 = 还原前合计 1400，四项合计 100.00%（2 位舍入允许 ±0.01，如实暴露）
        assert_eq!(num(&group.restored.denominator), dec("1400"));
        assert_eq!(group.restored.material_share.as_deref(), Some("37.83"));
        assert_eq!(group.restored.labor_share.as_deref(), Some("31.09"));
        assert_eq!(group.restored.overhead_share.as_deref(), Some("31.09"));
        assert_eq!(group.restored.unresolved_share.as_deref(), Some("0.00"));
        assert_eq!(group.share_sum.as_deref(), Some("100.01"), "2 位舍入差如实下发，不凑整");
        // 占比变化（pp）与还原前占比
        assert_eq!(group.before.material_share.as_deref(), Some("71.43"));
        assert_eq!(group.before.labor_share.as_deref(), Some("14.29"));
        assert_eq!(group.shift.material_pp.as_deref(), Some("-33.60"));
        assert_eq!(group.shift.labor_pp.as_deref(), Some("16.80"));
        assert_eq!(group.shift.overhead_pp.as_deref(), Some("16.80"));
        assert_eq!(group.dominant, "material", "还原后材料仍是最大项");
        assert_eq!(num(&group.restored_labor), dec("235.2"), "从材料里拆出来的人工");
        assert_eq!(num(&group.restored_overhead), dec("235.2"));
        assert_eq!(num(&group.semi_in_material), dec("600"));
        assert_eq!(group.semi_share.as_deref(), Some("60.00"));
        assert_eq!(num(&group.check), Decimal::ZERO, "报表守恒自检");
        assert_eq!(group.restored_rows, 1);
        assert_eq!(group.partial_rows, 0);

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ② 三级还原：`max_depth` 1→4 时三张表**同步**停在那一层（含结构表「不报截断」的口径差）。
    #[tokio::test]
    async fn e2_depth_limit_stops_the_chain_at_the_same_layer_in_all_tables() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let keyword = fixture.tag("F");
        let period = &fixture.periods[3];
        let principal = fixture.super_principal();
        let object_no = fixture.object_no("F");

        // (max_depth, 不彻底, 外购料, 人工, 制造费用)
        let expected = [
            (1_u32, "360", "400", "320", "320"),
            (2, "216", "400", "392", "392"),
            (3, "129.6", "400", "435.2", "435.2"),
            (4, "0", "529.6", "435.2", "435.2"),
        ];
        for (depth, unresolved, material, labor, overhead) in expected {
            let s_res = structure(&principal, period, &keyword, Some(depth)).await;
            let s_row = structure_row(&s_res, &object_no);
            let r_res = rollup(&principal, period, &keyword, Some(depth)).await;
            let r_row = rollup_row(&r_res, &object_no);

            assert_eq!(s_row.depth, depth, "结构表 depth = max_depth={depth}");
            assert_eq!(s_row.layers.len(), usize::try_from(depth).unwrap_or(0));
            assert_eq!(r_row.depth, depth, "还原表 depth 与结构表同步（max_depth={depth}）");
            assert_eq!(r_row.flows.len(), usize::try_from(depth).unwrap_or(0));

            assert_eq!(num(&r_row.unresolved), dec(unresolved), "max_depth={depth} 的不彻底");
            assert_eq!(num(&r_row.after.material), dec(material), "max_depth={depth} 的外购料");
            assert_eq!(num(&r_row.after.labor), dec(labor), "max_depth={depth} 的人工");
            assert_eq!(
                num(&r_row.after.overhead),
                dec(overhead),
                "max_depth={depth} 的制造费用"
            );
            assert_eq!(
                num(&r_row.after.total) + num(&r_row.unresolved),
                num(&r_row.before.total),
                "max_depth={depth} 守恒"
            );

            // 结构表状态：只在前两层报截断（下面是半成品）；第 3 层下面全是外购料 ⇒ 不报截断
            match depth {
                1 | 2 => assert_eq!(s_row.state, "truncated", "max_depth={depth} 结构表报截断"),
                3 => assert_eq!(s_row.state, "semi", "第 3 层下面全是外购料，结构到底了"),
                _ => assert_eq!(s_row.state, "semi"),
            }
            // 还原表状态：只要有金额停在半成品形态就是 partial
            if unresolved == "0" {
                assert_eq!(r_row.state, "restored");
            } else {
                assert_eq!(r_row.state, "partial", "max_depth={depth} 仍有不彻底");
                assert!(
                    r_res.note.contains("还原不彻底"),
                    "max_depth={depth} 页脚必须说清不彻底：{:?}",
                    r_res.note
                );
            }
        }

        // 第 3 层不报截断，但还原表仍差一层：这是两张表的口径差，不是 bug
        let three = rollup(&principal, period, &keyword, Some(3)).await;
        let three_row = rollup_row(&three, &object_no);
        let stopped = three_row
            .semis
            .iter()
            .find(|semi| semi.stopped)
            .expect("第 3 层 S4 停住");
        assert_eq!(stopped.material_code.as_deref(), Some(chain.semi_s4.as_str()));
        assert_eq!(num(&stopped.unresolved), dec("129.6"));
        let structure_three = structure(&principal, period, &keyword, Some(3)).await;
        let structure_row_three = structure_row(&structure_three, &object_no);
        assert!(
            structure_row_three.issues.is_empty(),
            "结构表不该报警：{:?}",
            structure_row_three.issues
        );

        // 报表跟着同一档：max_depth=3 的报表分组仍与明细表逐字段一致
        let rep_three = report(&principal, period, &keyword, "object", "all", Some(3)).await;
        let group_three = group_of(&rep_three, &chain.object_f.to_string());
        assert_eq!(num(&group_three.restored.material), num(&three_row.after.material));
        assert_eq!(num(&group_three.restored.unresolved), num(&three_row.unresolved));
        assert_eq!(group_three.partial_rows, 1);
        assert_eq!(group_three.restored_rows, 0);
        // 不彻底在报表里占一格 ⇒ 四项合计仍约 100%
        let share_sum = num(group_three.share_sum.as_deref().expect("四项占比都有值"));
        assert!(
            (share_sum - dec("100")).abs() <= dec("0.01"),
            "四项占比合计 {share_sum} 应落在 100.00±0.01"
        );

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ③ 三档状态在两张表之间一致；`no_semi` 对照对象不被还原。
    #[tokio::test]
    async fn e3_states_line_up_between_rollup_and_report() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let keyword = fixture.mark();
        let period = &fixture.periods[3];
        let principal = fixture.super_principal();

        let roll = rollup(&principal, period, &keyword, None).await;
        assert_eq!(roll.total, 2, "本期间两个对象：F（四层链）+ G（全外购料）");
        let f_row = rollup_row(&roll, &fixture.object_no("F"));
        let g_row = rollup_row(&roll, &fixture.object_no("G"));
        assert_eq!(
            g_row.product_code.as_deref(),
            Some(chain.finished_g.as_str()),
            "对照对象是成品 G"
        );
        assert_eq!(f_row.state, "restored");
        assert_eq!(g_row.state, "no_semi");
        assert_eq!(num(&g_row.unresolved), Decimal::ZERO);
        assert_eq!(
            num(&g_row.after.material),
            num(&g_row.before.material),
            "全外购料：还原前后一致"
        );
        assert_eq!(g_row.semis.len(), 0, "没有半成品明细");

        // 报表三档行数与还原表同数
        let rep = report(&principal, period, &keyword, "dept", "all", None).await;
        assert_eq!(rep.total, 1, "同一车间只有一个分组");
        let group = group_of(&rep, &fixture.dept.to_string());
        assert_eq!(group.object_count, 2);
        assert_eq!(group.restored_rows, roll.totals.restored_row_count);
        assert_eq!(group.partial_rows, roll.totals.partial_row_count);
        assert_eq!(group.no_semi_rows, roll.totals.no_semi_row_count);
        assert_eq!(group.no_semi_rows, 1);
        // 报表合计 = 明细表整表合计（两个接口同一批行）
        assert_eq!(num(&group.before.total), num(&roll.totals.before_total));
        assert_eq!(num(&group.restored.material), num(&roll.totals.after_material));
        assert_eq!(num(&group.restored.labor), num(&roll.totals.after_labor));
        assert_eq!(num(&group.restored.overhead), num(&roll.totals.after_overhead));
        assert_eq!(num(&group.check), Decimal::ZERO);
        // 合计 = F + G（手算）
        assert_eq!(num(&group.before.material), dec("1500"), "1000 + 500");
        assert_eq!(num(&group.restored.material), dec("1029.6"), "529.6 + 500");
        assert_eq!(num(&group.restored.labor), dec("535.2"), "435.2 + 100");
        assert_eq!(num(&group.restored.overhead), dec("535.2"));
        assert_eq!(num(&group.before.total), dec("2100"));

        // state 筛选：restored 只剩 F，no_semi 只剩 G（两张表同步收窄）
        let only_restored = report(&principal, period, &keyword, "object", "restored", None).await;
        assert_eq!(only_restored.total, 1);
        assert_eq!(group_of(&only_restored, &chain.object_f.to_string()).object_count, 1);
        let only_no_semi = report(&principal, period, &keyword, "object", "no_semi", None).await;
        assert_eq!(only_no_semi.total, 1);
        assert_eq!(group_of(&only_no_semi, &chain.object_g.to_string()).object_count, 1);
        let roll_restored = rollup(&principal, period, &keyword, None).await;
        assert_eq!(
            roll_restored.totals.row_count, 2,
            "还原表本身不受 state 影响（这里用 all）"
        );

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ④ 四档维度汇总：分组之和 = 整表合计，且每一行都等于明细表对应行之和。
    #[tokio::test]
    async fn e4_report_groupings_sum_to_the_whole_table_on_the_chain() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let keyword = fixture.mark();
        let period = &fixture.periods[3];
        let principal = fixture.super_principal();

        let roll = rollup(&principal, period, &keyword, None).await;
        let rows: Vec<&RollupRow> = roll.list.iter().collect();
        assert_eq!(rows.len(), 2);

        for group_by in ["dept", "product", "object", "all"] {
            let rep = report(&principal, period, &keyword, group_by, "all", None).await;
            // 分组之和 = 整表合计（同一个累加器，构造性成立）
            let mut before_material = Decimal::ZERO;
            let mut after_material = Decimal::ZERO;
            let mut after_labor = Decimal::ZERO;
            let mut after_overhead = Decimal::ZERO;
            let mut unresolved = Decimal::ZERO;
            let mut objects = 0_u64;
            for group in &rep.groups {
                before_material += num(&group.before.material);
                after_material += num(&group.restored.material);
                after_labor += num(&group.restored.labor);
                after_overhead += num(&group.restored.overhead);
                unresolved += num(&group.restored.unresolved);
                objects += group.object_count;
                assert_eq!(num(&group.check), Decimal::ZERO, "{group_by} 分组守恒");
            }
            assert_eq!(
                objects,
                u64::try_from(rows.len()).unwrap_or(0),
                "{group_by}：对象不能重复/遗漏"
            );
            assert_eq!(
                before_material,
                num(&rep.totals.before.material),
                "{group_by} 还原前材料"
            );
            assert_eq!(
                after_material,
                num(&rep.totals.restored.material),
                "{group_by} 还原后材料"
            );
            assert_eq!(after_labor, num(&rep.totals.restored.labor), "{group_by} 还原后人工");
            assert_eq!(
                after_overhead,
                num(&rep.totals.restored.overhead),
                "{group_by} 还原后制造费用"
            );
            assert_eq!(unresolved, num(&rep.totals.restored.unresolved), "{group_by} 不彻底");
            // 整表合计 = 明细表合计
            assert_eq!(
                num(&rep.totals.before.material),
                num(&roll.totals.before_material),
                "{group_by}"
            );
            assert_eq!(
                num(&rep.totals.restored.total),
                num(&roll.totals.after_total),
                "{group_by}"
            );
        }

        // 整表合计 = 两个对象逐字段相加（手算）
        let rep_all = report(&principal, period, &keyword, "all", "all", None).await;
        let totals = &rep_all.totals;
        assert_eq!(num(&totals.before.material), dec("1500"));
        assert_eq!(num(&totals.before.labor), dec("300"));
        assert_eq!(num(&totals.before.overhead), dec("300"));
        assert_eq!(num(&totals.before.total), dec("2100"));
        assert_eq!(num(&totals.restored.material), dec("1029.6"));
        assert_eq!(num(&totals.restored.labor), dec("535.2"));
        assert_eq!(num(&totals.restored.overhead), dec("535.2"));
        assert_eq!(num(&totals.restored.total), dec("2100"));
        assert_eq!(num(&totals.restored.unresolved), Decimal::ZERO);
        assert_eq!(num(&totals.check), Decimal::ZERO);
        assert_eq!(num(&group_of(&rep_all, "ALL").restored.material), dec("1029.6"));

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ⑤ 数据范围：别车间在三张表里都看不到本车间的行（不是 403，是空表）。
    #[tokio::test]
    async fn e5_other_dept_sees_nothing_in_any_table() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let keyword = fixture.mark();
        let period = &fixture.periods[3];
        let other = fixture.other_dept_principal();

        let s_res = structure(&other, period, &keyword, None).await;
        let r_res = rollup(&other, period, &keyword, None).await;
        let rep = report(&other, period, &keyword, "all", "all", None).await;
        assert_eq!(s_res.total, 0, "结构表看不到别车间的对象");
        assert_eq!(r_res.total, 0, "还原表看不到别车间的对象");
        assert_eq!(rep.total, 0, "报表连分组都不该出现");
        assert_eq!(r_res.totals.row_count, 0);
        assert_eq!(num(&rep.totals.before.total), Decimal::ZERO);
        assert_eq!(num(&rep.totals.check), Decimal::ZERO);
        assert!(rep.totals.restored.material_share.is_none(), "空表不画假 0.00%");

        // 本车间能看到（证明上面的 0 是数据范围，不是「查不到」）
        let mine = rollup(&fixture.super_principal(), period, &keyword, None).await;
        assert_eq!(mine.total, 2);

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// zz 清场：本夹具按前缀清完，库里零残留。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }
}
