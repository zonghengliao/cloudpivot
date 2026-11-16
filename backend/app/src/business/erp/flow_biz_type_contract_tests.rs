//! 审批类型（`FlowBizType`）与应用内写入点的契约（P2-29）。
//!
//! 这两条要 import **ERP 代码**（`approval_gate` 与各 `*_bridge`），所以它们跟着 ERP 走、
//! 住在应用 crate 里，不进零依赖的 `cloudpivot-doc` crate —— 反过来把它们塞进
//! `business::shared` 才会把 `shared → erp` 的回边长回来（P2-B 解的就是这条）。
//!
//! | 用例 | 钉住什么 |
//! |---|---|
//! | [`flow_health_list_matches_registry`] | `approval_gate::FLOW_TYPES` ↔ `FlowBizType::HEALTH_CHECKED` 逐条一致 |
//! | [`approval_entries_use_registered_biz_types`] | 各 `*_bridge::BIZ_TYPE` 都是注册过的枚举值 |
//!
//! 词表侧的不变量（拼写规则 / 轴内唯一 / 值域 ↔ 枚举 / `DocKind::route()` 全覆盖）在
//! `cloudpivot-doc` crate 的 `vocab_tests.rs`；词表 ↔ 前端镜像 / 演示数据 / 巡检 SQL 的契约在
//! 同 crate 的 `contract_tests.rs`。

use super::approval_gate;
use super::{
    contract::contract_bridge,
    cost::cost_period_bridge,
    payable::payable_bridge,
    purchase::{purchase_bridge, purchase_return_bridge},
    receipt_refund_bridge, requisition_bridge,
    sales::quotation_approval_bridge,
    sales::sales_approval_bridge,
    stockin::stockin_query,
    stocktake::stocktake_bridge,
    voucher::voucher_bridge,
};
use cloudpivot_doc::FlowBizType;

/// `approval_gate::FLOW_TYPES`（健康检查的 8 个）与 `FlowBizType::HEALTH_CHECKED` 逐条一致。
#[test]
fn flow_health_list_matches_registry() {
    let gate: Vec<(&str, &str)> = approval_gate::FLOW_TYPES.to_vec();
    let mine: Vec<(&str, &str)> = FlowBizType::HEALTH_CHECKED
        .iter()
        .map(|item| (item.as_str(), item.flow_code()))
        .collect();
    assert_eq!(gate, mine, "审批流健康检查清单与词表不一致");
}

/// 各模块写审批用的 `BIZ_TYPE` 常量都必须是注册过的枚举值。
#[test]
fn approval_entries_use_registered_biz_types() {
    let registered: Vec<&str> = FlowBizType::ALL.iter().map(|item| item.as_str()).collect();
    for (name, value) in [
        ("purchase_bridge::BIZ_TYPE", purchase_bridge::BIZ_TYPE),
        ("stockin_query::POST_BIZ_TYPE", stockin_query::POST_BIZ_TYPE),
        ("stockin_query::CANCEL_BIZ_TYPE", stockin_query::CANCEL_BIZ_TYPE),
        ("payable_bridge::BIZ_TYPE", payable_bridge::BIZ_TYPE),
        ("payable_bridge::CANCEL_BIZ_TYPE", payable_bridge::CANCEL_BIZ_TYPE),
        ("voucher_bridge::BIZ_TYPE", voucher_bridge::BIZ_TYPE),
        ("contract_bridge::BIZ_TYPE", contract_bridge::BIZ_TYPE),
        ("stocktake_bridge::BIZ_TYPE", stocktake_bridge::BIZ_TYPE),
        // t_c49010bf：这 4 个 bridge 的 BIZ_TYPE 当年没进本清单，于是
        // `biz_flow_def` 里长出 4 个域外值而无人拦（闸④ 在全量库上必红）。
        // 补进来的判据不变：字面量必须在 `FlowBizType` 域内。
        ("sales_approval_bridge::BIZ_TYPE", sales_approval_bridge::BIZ_TYPE),
        ("requisition_bridge::BIZ_TYPE", requisition_bridge::BIZ_TYPE),
        ("purchase_return_bridge::BIZ_TYPE", purchase_return_bridge::BIZ_TYPE),
        ("cost_period_bridge::BIZ_TYPE", cost_period_bridge::BIZ_TYPE),
        ("receipt_refund_bridge::BIZ_TYPE", receipt_refund_bridge::BIZ_TYPE),
        // M3-04：报价单审批（发出前置）—— 新审批线一律进这张清单，否则
        // `biz_flow_def` 又会长出域外值而无人拦（t_c49010bf 的教训）。
        (
            "quotation_approval_bridge::BIZ_TYPE",
            quotation_approval_bridge::BIZ_TYPE,
        ),
    ] {
        assert!(
            registered.contains(&value),
            "{name} = {value:?} 不在 FlowBizType 域内（新增业务类型必须先登记）"
        );
    }
}
