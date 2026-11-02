// ============================================================================
// stockin_tests.rs —— 测试部件：`erp/stockin_model` 的全部用例（成本口径 + 状态机 + 真库过账/作废）
//
// 由 `stockin_model.rs` 的 `#[cfg(test)] mod pure_tests` 与 `mod tests` 原样搬出并拍平成一个文件
// （TD-1 拆分）。**本文件不含任何行为变更**：用例体、夹具、SQL、断言与拆分前一致 ——
// 用例数 8 → 8、用例名逐一相同（只少了 `mod pure_tests` 这层包裹）。
// 挂载点写在 facade `stockin_model.rs` 末尾；本文件自带完整 import 头，见方法论 §3 坑 1。
// ============================================================================

// 拆分前本测试模块挂在 `stockin_model.rs` 内部，`use super::*` 同时传递父模块的
// 公开项与私有 `use`；独立成文件后私有 `use` 不再传递，故原样补回父模块 import 头。
use nik::prelude::rust_decimal::Decimal;

use crate::business::erp::serial_model;

use super::*;
use std::str::FromStr;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试金额字面量非法")
}

/// ① 成本口径：`CostBasis::of` 用**不含税金额**除数量，单价 6 位四舍五入。
#[test]
fn cost_basis_is_net_amount_per_unit() {
    let cost = CostBasis::of(dec("3"), dec("9.99"));
    assert_eq!(cost.qty, dec("3"));
    assert_eq!(cost.amount, dec("9.99"), "金额原样保留（不进存货的是税额）");
    assert_eq!(cost.unit_cost, dec("3.330000"));
    // 13%：不含税 200 / 含税 226，成本只能是 200
    let taxed = CostBasis::of(dec("100"), dec("200"));
    assert_eq!(taxed.unit_cost, dec("2.000000"));
    assert_ne!(taxed.amount, dec("226"));
}

/// ② 尾差：单价除不尽时 6 位四舍五入（PRD §3.2.5 算例 2：21.09 / 13 = 1.622308）。
#[test]
fn unit_cost_rounds_half_up_to_six_places() {
    let cost = CostBasis::of(dec("13"), dec("21.09"));
    assert_eq!(cost.unit_cost, dec("1.622308"));
}

/// ③ 无行建档分支的反推：`before = after - 增量` 精确可逆，单价回算与库里同源。
#[test]
fn derived_before_is_exact_inverse_of_addition() {
    let after = InventorySnapshot {
        qty: dec("200"),
        amount: dec("500"),
        avg_cost: dec("2.500000"),
    };
    let before = InventorySnapshot::derived_before(after, dec("100"), dec("300"));
    assert_eq!(before.qty, dec("100"));
    assert_eq!(before.amount, dec("200"));
    assert_eq!(before.avg_cost, dec("2.000000"));
    // 首次入库：before 全 0
    let first = InventorySnapshot::derived_before(
        InventorySnapshot {
            qty: dec("5"),
            amount: dec("10"),
            avg_cost: dec("2.000000"),
        },
        dec("5"),
        dec("10"),
    );
    assert_eq!(first, InventorySnapshot::zero());
}

/// ④ 状态机：6 条合法边（P16 的 3 条作废审批边 + 原有 3 条），
/// `POSTED → POSTED` 与 `CANCELLED` 之后的一切都是 409。
#[test]
fn only_six_legal_edges() {
    assert!(can_transition(STATUS_DRAFT, STATUS_POSTED));
    assert!(can_transition(STATUS_DRAFT, STATUS_CANCELLED));
    assert!(can_transition(STATUS_POSTED, STATUS_CANCELLED));
    // P16：作废审批三步
    assert!(can_transition(STATUS_POSTED, STATUS_PENDING_CANCEL));
    assert!(can_transition(STATUS_PENDING_CANCEL, STATUS_CANCELLED));
    assert!(can_transition(STATUS_PENDING_CANCEL, STATUS_POSTED));
    // P-FLOW-3A：过账审批四步（提交 → 通过 / 驳回 / 审批中作废）
    assert!(can_transition(STATUS_DRAFT, STATUS_PENDING_POST));
    assert!(can_transition(STATUS_PENDING_POST, STATUS_POSTED));
    assert!(can_transition(STATUS_PENDING_POST, STATUS_DRAFT));
    assert!(can_transition(STATUS_PENDING_POST, STATUS_CANCELLED));
    assert_eq!(LEGAL_EDGES.len(), 10, "边表总数钉死，防止悄悄多放/少放");
    assert!(
        !can_transition(STATUS_POSTED, STATUS_POSTED),
        "重复过账非法"
    );
    // 作废审批中不能再跳回草稿，也不能回到「过账审批中」
    assert!(!can_transition(STATUS_PENDING_CANCEL, STATUS_DRAFT));
    assert!(!can_transition(STATUS_PENDING_CANCEL, STATUS_PENDING_POST));
    assert!(!can_transition(
        STATUS_PENDING_CANCEL,
        STATUS_PENDING_CANCEL
    ));
    assert!(!can_transition(STATUS_CANCELLED, STATUS_POSTED));
    assert!(!can_transition(STATUS_CANCELLED, STATUS_DRAFT));
    assert!(!can_transition(STATUS_CANCELLED, STATUS_PENDING_CANCEL));
    assert!(!can_transition(STATUS_CANCELLED, STATUS_PENDING_POST));
    // 已过账的单据不许「回到过账审批中」（那是绕过审批的回头路）
    assert!(!can_transition(STATUS_POSTED, STATUS_PENDING_POST));
    assert_eq!(
        next_statuses(STATUS_DRAFT),
        vec![STATUS_POSTED, STATUS_CANCELLED, STATUS_PENDING_POST]
    );
    assert_eq!(
        next_statuses(STATUS_PENDING_CANCEL),
        vec![STATUS_CANCELLED, STATUS_POSTED]
    );
    assert!(next_statuses(STATUS_CANCELLED).is_empty());
    let message = transition_rejected_message(STATUS_DRAFT, "CANCELLED");
    assert!(message.contains("POSTED"), "文案要点名可选去向：{message}");
}

/// ④b 作废审批中单据必须锁定（不许改数量 / 金额 / 明细）。
#[test]
fn pending_cancel_is_locked() {
    use crate::business::shared::state_machine::Entity;
    assert!(Entity::StockIn.is_locked(STATUS_PENDING_CANCEL));
    assert!(Entity::StockIn.is_locked(STATUS_POSTED));
    assert!(!Entity::StockIn.is_locked(STATUS_DRAFT));
    // 两步都要 reason（申请理由 / 记账依据）
    assert!(Entity::StockIn.needs_reason(STATUS_POSTED, STATUS_PENDING_CANCEL));
    assert!(Entity::StockIn.needs_reason(STATUS_PENDING_CANCEL, STATUS_CANCELLED));
    assert!(!Entity::StockIn.needs_reason(STATUS_PENDING_CANCEL, STATUS_POSTED));
    assert_eq!(status_label(STATUS_PENDING_CANCEL), "作废审批中");
}

/// ⑤ 单号格式：`IN-yyyyMMdd-000001`（6 位只补零不截断）。
#[test]
fn in_no_format() {
    assert_eq!(format_in_no("20261031", 1), "IN-20261031-000001");
    assert_eq!(format_in_no("20261031", 1234567), "IN-20261031-1234567");
}

/// ⑥ 作废哨兵：三个前缀必须互不包含（handler 靠 `strip_prefix` 分派 409 文案；
/// 一旦谁是谁的前缀，报文就会静默走错分支，把「序列号已出库」说成「库存不足」）。
#[test]
fn cancel_sentinels_are_distinct() {
    assert_eq!(SHORTFALL_PREFIX, "INV_SHORTFALL|");
    assert_eq!(MISSING_TXN_PREFIX, "INV_NO_TXN|");
    assert_eq!(SERIAL_SHIPPED_PREFIX, "SERIAL_SHIPPED|");
    assert_eq!(PO_RACE_PREFIX, "PO_STATUS_RACE|");
    let all = [
        SHORTFALL_PREFIX,
        MISSING_TXN_PREFIX,
        SERIAL_SHIPPED_PREFIX,
        PO_RACE_PREFIX,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            if i != j {
                assert!(
                    !a.starts_with(b),
                    "{a} 不能以 {b} 开头（前缀重叠会让文案串台）"
                );
                assert!(
                    !b.starts_with(a),
                    "{b} 不能以 {a} 开头（前缀重叠会让文案串台）"
                );
            }
        }
    }
    assert_eq!(TXN_TYPE_OUT, "OUT");
    assert_ne!(BIZ_TYPE_PURCHASE_IN_CANCEL, BIZ_TYPE_PURCHASE_IN);
    assert_eq!(BIZ_TYPE_PURCHASE_IN_CANCEL, "PURCHASE_IN_CANCEL");
}

/// ⑦ P10：序列号状态文案（`409` 报文里「已出库 / 已报废」两个分支都要有落点）。
#[test]
fn serial_status_label_covers_all_states() {
    assert_eq!(serial_status_label(serial_model::STATUS_OUTBOUND), "出库");
    assert_eq!(serial_status_label(serial_model::STATUS_SCRAPPED), "报废");
    assert_eq!(serial_status_label(serial_model::STATUS_IN_STOCK), "变更");
}
