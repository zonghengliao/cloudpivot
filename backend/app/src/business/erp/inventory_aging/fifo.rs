//! FIFO 层分配（纯逻辑，不碰库）—— 库龄口径的**参考实现**。
//!
//! 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））。
//!
//! # 为什么要有第二份实现
//!
//! 生产路径的 FIFO 在 SQL 里（[`super::model`] 的窗口函数：`GREATEST(0, LEAST(qty,
//! cum_in − out_total))`）——那是一行集合运算，跑得快，但「对不对」不是肉眼能看出来的。
//! 这里用**最直白的循环**再写一遍同一件事：最早入库的层先被吃、吃满再吃下一层。
//!
//! 两份实现互证是这类「累加 + 夹 0」逻辑唯一可靠的验收方式（真库用例
//! `aging_db_tests::sql_matches_reference_allocator` 拿同一批夹具逐层对拍）：
//! SQL 的窗口函数写错（比如把 `cum_in` 写成 `cum_out`、或漏了 `LEAST` 的夹上限），
//! 单独看断言往往还是绿的，只有和另一份实现比才会露。
//!
//! # 它不是什么
//!
//! **不是** `stockout::stockout_model::allocate_fifo`：那是「过账时从哪个库位桶扣货」的
//! 库位选择 FIFO（排序键是库位账行的 `created_at`），和库龄层无关。两个 FIFO 名字一样、
//! 对象不同，别互相替代。
//!
//! # 口径（与 SQL 逐条对应）
//!
//! 1. 输入是**同一 `物料 × 仓库`** 的入库层，按 `(入库日期, 过账时间, id)` 从老到新排好；
//! 2. 出库总量是这一格在基准日之前的**全部** OUT 数量和（恒正、按 FIFO 由老到新消耗）；
//! 3. 某层剩余 = `原量 − 被吃掉的量`，被吃掉的量 = `min(还没摊完的出库量, 本层原量)`；
//! 4. 出库量比入库量还大（脏数据 / 负库存仓）时：所有层剩余 0，**不编造负数层**。

use nik::prelude::rust_decimal::Decimal;

/// 把累计出库量按 FIFO 摊到各入库层上，返回每层的**剩余量**（顺序与输入一致）。
///
/// # 参数
///
/// - `inflows`：各层入库数量，按「入库时间从老到新」排好序；`<= 0` 的层按 0 处理
///   （流水 `ck_biz_inv_txn_qty` 已经保证 `qty > 0`，这里只是防御）。
/// - `total_out`：这一格（`物料 × 仓库`）在基准日之前的出库总量；负数按 0 处理。
///
/// # 返回
///
/// 每层剩余量，恒 `0 <= 剩余 <= 原量`；`Σ剩余 = max(0, Σ入库 − 出库)`。
pub fn allocate_fifo(inflows: &[Decimal], total_out: Decimal) -> Vec<Decimal> {
    let mut left_to_consume = if total_out > Decimal::ZERO {
        total_out
    } else {
        Decimal::ZERO
    };
    let mut remaining = Vec::with_capacity(inflows.len());
    for inflow in inflows {
        let qty = if *inflow > Decimal::ZERO {
            *inflow
        } else {
            Decimal::ZERO
        };
        let consumed = if left_to_consume < qty { left_to_consume } else { qty };
        left_to_consume -= consumed;
        remaining.push(qty - consumed);
    }
    remaining
}

/// `Σ` 一个数量集合（层剩余量 / 入库量都适用）。
pub fn sum_of(values: &[Decimal]) -> Decimal {
    values.iter().copied().fold(Decimal::ZERO, |acc, value| acc + value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn texts(values: &[Decimal]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// 教科书场景：老的先吃满，吃到一半的那层留余光。
    #[test]
    fn oldest_layer_is_consumed_first() {
        let layers = [dec("100"), dec("200"), dec("50")];
        let remaining = allocate_fifo(&layers, dec("180"));
        assert_eq!(texts(&remaining), ["0", "120", "50"]);
        println!("[M2-08] FIFO：100/200/50 出 180 → 剩 0/120/50 ✓");
    }

    /// 出库量正好等于某层累计（边界：不该「多吃一层」或「少吃一层」）。
    #[test]
    fn exact_boundaries_do_not_bleed() {
        let layers = [dec("100"), dec("200")];
        assert_eq!(texts(&allocate_fifo(&layers, dec("0"))), ["100", "200"]);
        assert_eq!(texts(&allocate_fifo(&layers, dec("100"))), ["0", "200"]);
        assert_eq!(texts(&allocate_fifo(&layers, dec("299.9999"))), ["0", "0.0001"]);
        assert_eq!(texts(&allocate_fifo(&layers, dec("300"))), ["0", "0"]);
        println!("[M2-08] FIFO 边界：0 / 100 / 299.9999 / 300 ✓");
    }

    /// 出库比入库还多（脏数据 / 负库存仓）：剩余全 0，不编造负数层。
    #[test]
    fn over_consumption_leaves_no_negative_layer() {
        let layers = [dec("10"), dec("20")];
        let remaining = allocate_fifo(&layers, dec("100"));
        assert_eq!(texts(&remaining), ["0", "0"]);
        assert_eq!(sum_of(&remaining), Decimal::ZERO);
        println!("[M2-08] 出库 > 入库 → 剩余全 0（不编负数）✓");
    }

    /// 不变式（随机语料）：每层 0 ≤ 剩 ≤ 原量，Σ剩 = max(0, Σ入 − 出)。
    #[test]
    fn invariants_hold_on_generated_cases() {
        let mut seed: u64 = 0x2026_1116;
        let mut next = |bound: u64| {
            // 线性同余，够用来造一批不重复的组合（不引第三方）
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % bound
        };
        for _ in 0..200 {
            let count = 1 + next(6) as usize;
            let layers: Vec<Decimal> = (0..count).map(|_| dec(&format!("{}", 1 + next(500)))).collect();
            let out = dec(&format!("{}", next(1_500)));
            let remaining = allocate_fifo(&layers, out);
            for (inflow, left) in layers.iter().zip(remaining.iter()) {
                assert!(
                    *left >= Decimal::ZERO && left <= inflow,
                    "层剩余越界：{left} ∈ [0, {inflow}]"
                );
            }
            let total_in = sum_of(&layers);
            let want = if total_in > out { total_in - out } else { Decimal::ZERO };
            assert_eq!(
                sum_of(&remaining),
                want,
                "Σ剩余 != max(0, Σ入 − 出)（层 {layers:?}，出 {out}）"
            );
        }
        println!("[M2-08] FIFO 不变式 200 组随机语料全过 ✓");
    }

    /// 零层 / 空层不 panic（没有入库流水的物料就是这种形态）。
    #[test]
    fn empty_inputs_are_safe() {
        assert!(allocate_fifo(&[], dec("10")).is_empty());
        assert_eq!(texts(&allocate_fifo(&[dec("0")], dec("0"))), ["0"]);
        assert_eq!(sum_of(&[]), Decimal::ZERO);
        println!("[M2-08] 空层 / 零层安全 ✓");
    }
}
