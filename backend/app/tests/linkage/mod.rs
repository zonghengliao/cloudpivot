//! 跨模块联动矩阵测试套件（入口 `backend/tests/linkage.rs`）。
//!
//! # 设计方法（本卡的核心）
//!
//! 单模块测试回答「这个模块自己对不对」；联动测试回答「A 变了，B 有没有跟着变」。
//! 后者按一张四列表格穷举：
//!
//! ```text
//! | 主体变更                | 谁引用了它              | 引用者该变成       | 撤销函数                    |
//! |------------------------|------------------------|-------------------|----------------------------|
//! | 任务 → CANCELLED        | biz_flow_instance      | CANCELLED         | cancel_pending_of_biz      |
//! |                        | biz_flow_task          | CANCELLED + 时间   | （同一个）                   |
//! |                        | biz_flow_instance_node | CANCELLED         | （同一个）                   |
//! |                        | 子任务                  | CANCELLED         | cascade_cancel_descendants |
//! | 收货单 POSTED→CANCELLED | biz_inventory          | qty 减回           | reverse_cost               |
//! |                        | biz_inventory_location | qty 减回           | reverse_location_qty       |
//! |                        | biz_inventory_txn      | 追加 OUT 冲销       | write_cancel_txn           |
//! |                        | biz_voucher            | 红冲               | reverse                    |
//! |                        | biz_payable            | CANCELLED         | cancel                     |
//! |                        | biz_material_serial    | 软删               | void_serials_of            |
//! | 调拨 PENDING→CANCELLED  | biz_flow_instance      | CANCELLED         | cancel_pending_of_biz      |
//! |                        | 库存                    | 不动（未结算）      | -                          |
//! | 应付 SETTLED            | 凭证                    | 生成付款凭证        | -                          |
//! ```
//!
//! 每一行都要有「正向 + 反向」配对：正向证明该发生的发生了，反向证明**不该**动的没动
//! （反向用例才是真正抓「孤儿待办」这类缺陷的那一半）。

pub mod common;
pub mod payable_chain;
pub mod stockin_chain;
pub mod task_flow_linkage;
pub mod transfer_chain;
pub mod voucher_chain;
