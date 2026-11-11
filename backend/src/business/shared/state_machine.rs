//! 统一状态机（批次 2 · P10 · 老六「五条铁律」之 ③④）。
//!
//! 依据卡面 §二：**所有单据的状态变更必须走同一个入口**，入口负责
//! 「边表校验 → reason 校验 → 写 `biz_status_log`」，副作用（加库存 / 生成凭证）
//! 由调用方在**同一个事务**里接着做。
//!
//! # 为什么要有这个文件（开工实测的三个洞）
//!
//! ```text
//! ① 状态机可被绕过：
//!    purchase_model::LEGAL_EDGES 只有 4 条边，而 RECEIVED 是
//!    stockin_model::sync_purchase_status 用一句 `UPDATE … SET status = CASE …`
//!    直接写出来的 —— 完全没走 can_transition()。
//!    绕过一次 = 状态机不存在。
//! ② 没有状态变更日志：五张单据表只有 created_by / update_by / updated_at，
//!    谁在什么时候改了状态查不到。
//! ③ 入账后还能改：POSTED / RECEIVED / SETTLED 状态下金额、数量、明细没有硬约束。
//! ```
//!
//! # 三个公开能力
//!
//! | 函数 | 职责 |
//! |------|------|
//! | [`transition`] | 唯一状态变更入口：校验 + 写日志 |
//! | [`transition_if_changed`] | 同上，但**状态没变就跳过**（业务性推进专用，返回 `Ok(None)`） |
//! | [`log_created`] | 建单留痕（`from_status = NULL`） |
//! | [`ensure_writable`] | 入账态只读闸（各 `update_*` 入口调用） |
//!
//! # 边表的唯一真相
//!
//! 五个实体的合法边、需要 reason 的边、入账态清单都写在 [`Entity`] 里，
//! 各 model 保留自己的 `LEGAL_EDGES` 常量（指向本文件的常量数组），
//! [`transition`] 会**核对**传进来的 `edges` 与注册表完全一致 ——
//! 谁想「顺手多放一条边」会立刻拿到 500 而不是静默放行（硬红线 5 的机器化）。
//!
//! # 日志只增不改
//!
//! `biz_status_log`（V0159）没有 `deleted_at` / `updated_at`，代码里也只有 INSERT：
//! 日志与业务变更同事务，任一方失败整笔回滚，「业务改了没日志」不可能出现。
//!
//! # 自环
//!
//! `from == to` **不是状态变更**（如 `PARTIAL_RECEIVED → PARTIAL_RECEIVED` 继续部分收货）。
//! [`transition`] 一律拒绝自环并给出 409 —— 它只写「真实变化」的日志；
//! 业务流程里正常的原地推进用 [`transition_if_changed`]（显式声明「允许没变」，
//! 状态相同则跳过、只留业务流水）。**别在调用点自己 `if from != to { transition() }`**
//! ——那种写法无法与本模块的注册表核对共存，也没法在代码里一眼看出「这里是故意的」。
//!
//! 反面教材（LNK-P08）：`payable_payment` 无条件调 `transition`，于是「分三期付款」的
//! 第二期 `PARTIAL_PAID → PARTIAL_PAID` 被 409 挡住 —— 正常业务路径直接断。

use std::collections::HashMap;
use std::result::Result;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

// ==================== 实体类型 ====================
//
// 落库值的**唯一真相**是 [`crate::business::shared::doc_kind::DocKind`]（P2-5 词表收敛）。
// 这里的常量是它在状态机侧的别名：值从枚举取，不再各写一遍字面量 ——
// 曾经 `ENTITY_TRANSFER = "TRANSFER"` 与本表 / `doc_type` / 审批 `biz_type` 三处的
// `STOCK_TRANSFER` 对不上，报表按 `entity_type` 分组就把调拨拆成两行（V0219 已改存量）。

use crate::business::shared::doc_kind::DocKind;

/// 实体类型：收货入库单。
pub const ENTITY_STOCK_IN: &str = DocKind::StockIn.as_str();
/// 实体类型：采购订单。
pub const ENTITY_PURCHASE_ORDER: &str = DocKind::PurchaseOrder.as_str();
/// 实体类型：库存调拨单。
///
/// **P2-5 收敛**：旧值 `TRANSFER` 与 `biz_stock_transfer` / `doc_type` / 审批
/// `biz_type` 三处的 `STOCK_TRANSFER` 不一致，已统一为 `STOCK_TRANSFER`。
pub const ENTITY_TRANSFER: &str = DocKind::StockTransfer.as_str();
/// 实体类型：应付单。
pub const ENTITY_PAYABLE: &str = DocKind::Payable.as_str();
/// 实体类型：凭证。
pub const ENTITY_VOUCHER: &str = DocKind::Voucher.as_str();
/// 实体类型：会计期间（`biz_accounting_period`）。
///
/// ERP-P1 起期间的开账 / 结账 / 反结账也写 `biz_status_log`：
/// 封账是**影响整月账务**的动作，谁封的、什么时候封的、为什么反结，
/// 必须与单据一样可追溯（卡面 L12 审计）。
pub const ENTITY_PERIOD: &str = DocKind::AccountingPeriod.as_str();

/// 实体类型：会计年度（年结，ERP-R4）。
///
/// 年结不是「某个月的期间流转变了」，而是**整个年度**的状态：
/// `NOT_CLOSED`（未结转） ↩ `CLOSED`（已结转）。`entity_no` 存**年度**（`2026`），
/// `entity_id` 存年结生成的结转凭证 id（空年度没有结转凭证时存随机 id —— 状态本身
/// 记在日志里，不靠凭证存在与否推断，否则「空年度年结成功」就没状态可查）。
///
/// 为什么单独一个实体而不是复用 `ACCOUNTING_PERIOD`：年结的边（`NOT_CLOSED ↔ CLOSED`）
/// 与期间的边（`NOT_OPENED / OPEN / CLOSED`）语义不同，混在一起会让
/// `PERIOD_EDGES` 变成两套语义的并集，任何一次期间结账都能「顺便」写成年度已结转。
pub const ENTITY_YEAR_END: &str = DocKind::AccountingYear.as_str();

/// 实体类型：合同（`biz_contract`，A1 合同表基座）。
///
/// 落库值取 `"CONTRACT"`（ARCH-2 附录 B #3 逐字规定；`biz_status_log.entity_type` 是
/// `VARCHAR(32)` 且无 CHECK 白名单，新实体登记后 `transition()` 即可用）。
pub const ENTITY_CONTRACT: &str = DocKind::Contract.as_str();

/// 实体类型：出库单（`biz_stock_out`，ARCH-3 DC-13 / R-P0-1）。
///
/// 补的是「普通物料没有任何出库单据」这个 P0 缺口：领用 / 报废 / 盘亏都走这张单，
/// 三本账负向变动 + 成本结转凭证。`biz_status_log.entity_type` 无 CHECK 白名单，
/// 本常量登记后 `transition()` 即可用；巡检 / 报表按它筛「出库轨迹」。
pub const ENTITY_STOCK_OUT: &str = DocKind::StockOut.as_str();

/// 实体类型：销售订单（`biz_sales_order`，销售链 · SO-04/SO-05）。
///
/// 落库值 `SALE_ORDER`（`DocKind::SaleOrder`）。状态机只在**单据头**上流转
/// （明细 `biz_sales_order_item` 没有状态列，`delivered_qty` 由出库过账回写）。
pub const ENTITY_SALES_ORDER: &str = DocKind::SaleOrder.as_str();

/// 实体类型：发货单（`biz_shipment`，销售链 · SO-08/SO-09）。
///
/// 落库值 `SHIPMENT`（`DocKind::Shipment`）。状态机只在**单据头**上流转
/// （明细 `biz_shipment_item` 没有状态列）；库存三本账由过账（SO-10）走
/// `inventory_writer` 写，本实体只管状态与 `biz_status_log` 轨迹。
pub const ENTITY_SHIPMENT: &str = DocKind::Shipment.as_str();

/// 实体类型：应收单（`biz_receivable`，销售链 · SO-13）。
///
/// 落库值 `RECEIVABLE`（`DocKind::Receivable`）。与 `ENTITY_PAYABLE` 是**镜像**关系：
/// 状态域不同（应收 5 值 `OPEN/PARTIAL/SETTLED/WRITTEN_OFF/CANCELLED`，应付是
/// `OPEN/PARTIAL_PAID/SETTLED/CANCELLED(+PENDING_CANCEL)`），边表也不同 ——
/// 见 [`RECEIVABLE_EDGES`]。`biz_receivable` 没有明细子表，状态只在单头流转。
pub const ENTITY_RECEIVABLE: &str = DocKind::Receivable.as_str();

/// 实体类型：收款单（`biz_receipt`，销售链 · SO-17）。
///
/// 落库值 `RECEIPT`（`DocKind::Receipt`）。**不是**应收的镜像：收款单是**资金单据**，
/// 走「登记 → 确认 → 过账」的台账流（ARCH-7 §4.4），状态域
/// `DRAFT / CONFIRMED / POSTED / CANCELLED` 与应收 / 应付都不是一套值。
/// 边表见 [`RECEIPT_EDGES`]；明细 `biz_receipt_allocation` 没有状态（属单头）。
pub const ENTITY_RECEIPT: &str = DocKind::Receipt.as_str();
/// 实体类型：收款退款单（`biz_receipt_refund`，销售链 · SO-14 遗留卡）。
///
/// 落库值 `RECEIPT_REFUND`（`DocKind::ReceiptRefund`）。资金单据的反向单据：
/// 与收款单同一套台账流（`DRAFT → CONFIRMED → POSTED`，`DRAFT → CANCELLED`），
/// 但**过账后不可作废**（钱已付出且凭证已记账）。边表见 [`RECEIPT_REFUND_EDGES`]；
/// 明细 `biz_receipt_refund_allocation` 没有状态（属单头）。
pub const ENTITY_RECEIPT_REFUND: &str = DocKind::ReceiptRefund.as_str();

/// 实体类型：销售发票（`biz_invoice`，销售链 · SO-19/SO-20）。
///
/// 落库值 `INVOICE`（`DocKind::Invoice`）。状态域 4 值（ARCH-7 §4.5）：
/// `DRAFT / ISSUED / POSTED / VOID` —— **`VOID` 不是收款单的 `CANCELLED`**，
/// 两词并存是稿面既定。边表见 [`INVOICE_EDGES`]：`ISSUED` 是「开票」（生成收入凭证 +
/// 回写应收），`POSTED` 是归档终态，作废从 `DRAFT / ISSUED` 进来（要原因）。
pub const ENTITY_INVOICE: &str = DocKind::Invoice.as_str();

/// 实体类型：销售退货单（`biz_sales_return`，销售链 · SO-29）。
///
/// 落库值 `SALES_RETURN`（`DocKind::SalesReturn`）。状态只在**单据头**上流转
/// （明细 `biz_sales_return_item` 没有状态列）；库存回库 / 应收冲减 / 红字凭证
/// 由过账卡（SO-30）在 `PENDING_POST → POSTED` 的同一个事务里完成，
/// 本实体只管状态与 `biz_status_log` 轨迹。
pub const ENTITY_SALES_RETURN: &str = DocKind::SalesReturn.as_str();

/// 实体类型：采购退货单（`biz_purchase_return`，采购链 · P2-03）。
///
/// 落库值 `PURCHASE_RETURN`（`DocKind::PurchaseReturn`）。状态只在**单据头**上流转
/// （明细 `biz_purchase_return_item` 没有状态列）；出库回冲 / 应付红字 / 供应商应退款 /
/// 冲销凭证由确认卡在同一事务里完成（`APPROVED → CONFIRMED`），本实体只管状态与轨迹。
pub const ENTITY_PURCHASE_RETURN: &str = DocKind::PurchaseReturn.as_str();

/// 出库单合法边（与 `stockout_base::LEGAL_EDGES` 同源）。
///
/// ```text
/// DRAFT          → PENDING_POST   提交待过账（只改状态，三本账一分没动）
/// PENDING_POST   → POSTED         过账（三本账负向 + 成本结转凭证，同一事务）
/// PENDING_POST   → DRAFT          撤回 / 驳回（打回草稿，账没动）
/// DRAFT          → POSTED         直接过账（制单人自己就是过账人的场景；受
///                                 erp.separation_of_duties.post 开关约束）
/// DRAFT          → CANCELLED      草稿作废（从未出过库，不动账）
/// POSTED         → CANCELLED      已过账作废（三本账回滚 + 凭证红冲，**必须给原因**）
/// ```
///
/// **没有** `PENDING_POST → CANCELLED`：待过账的单据从未动过账，
/// 作废它等价于「撤回 + 作废」两步，让状态机保持单向（少了这条边就少了
/// 「作废一张账没动的单却跑了回滚逻辑」的可能）。
pub const STOCK_OUT_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("PENDING_POST", "DRAFT"),
    ("DRAFT", "POSTED"),
    ("DRAFT", "CANCELLED"),
    ("POSTED", "CANCELLED"),
];

/// 需要 reason 的出库单边：已过账作废（红冲凭证的摘要与审计依据都靠它）。
pub const STOCK_OUT_REASON_EDGES: [(&str, &str); 1] = [("POSTED", "CANCELLED")];

/// 出库单入账态：已过账（数量 / 金额 / 明细都不许再改，更正只能作废重开）。
///
/// `PENDING_POST` 也锁：待过账状态下改一行数量，「别人批的 / 过的」就不是同一张单了。
pub const STOCK_OUT_LOCKED: [&str; 2] = ["POSTED", "PENDING_POST"];

/// 实体类型：盘点单（`biz_stock_take`，ARCH-3 **DC-14**）。
///
/// 补的是「账实差异只能手改 `biz_inventory`」这个 P1 缺口：盘盈盘亏没有单据、
/// 没有审批、没有凭证 → 审计上说不清「为什么库存变了」。`biz_status_log.entity_type`
/// 无 CHECK 白名单，本常量登记后 `transition()` 即可用。
pub const ENTITY_STOCK_TAKE: &str = DocKind::StockTake.as_str();

/// 盘点单合法边（与 `stocktake_base::LEGAL_EDGES` 同源）。
///
/// ```text
/// DRAFT          → PENDING_POST   提交审批（审批通过后仍是「待过账」）
/// PENDING_POST   → POSTED         过账（三本账按差异正负 + 盘点差异凭证，同一事务）
/// PENDING_POST   → DRAFT          撤回 / 驳回（打回草稿，账没动）
/// DRAFT          → CANCELLED      草稿作废（从未动过账）
/// PENDING_POST   → CANCELLED      待过账作废（账没动，顺带终止在跑的审批）
/// POSTED         → CANCELLED      已过账作废（三本账回滚 + 凭证红冲，**必须给原因**）
/// ```
///
/// **没有** `DRAFT → POSTED`：盘点必须先走审批（`stock_take_post`）才能过账，
/// 这正是 DC-14 要堵的「没人批就改账」。
pub const STOCK_TAKE_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("PENDING_POST", "DRAFT"),
    ("DRAFT", "CANCELLED"),
    ("PENDING_POST", "CANCELLED"),
    ("POSTED", "CANCELLED"),
];

/// 需要 reason 的盘点单边：已过账作废（红字凭证的摘要与审计依据都靠它）。
pub const STOCK_TAKE_REASON_EDGES: [(&str, &str); 1] = [("POSTED", "CANCELLED")];

/// 盘点单入账态：已过账（明细不许再改）；待过账也锁（审批过的那份差异不能被改薄）。
pub const STOCK_TAKE_LOCKED: [&str; 2] = ["POSTED", "PENDING_POST"];

// ==================== 预算（ARCH-6 第三章 · FIN-1） ====================

/// 实体类型：预算（`biz_budget`，ARCH-6 §3.3）。
pub const ENTITY_BUDGET: &str = DocKind::Budget.as_str();

/// 预算合法边（与 `budget_bridge::BUDGET_LEGAL_EDGES` / ARCH-6 §3.3.2 逐条一致）。
///
/// ```text
/// DRAFT          → SUBMITTED   提交审批（编制 / 调整都走这条）
/// SUBMITTED      → DRAFT       驳回 / 撤回（打回草稿，额度一分没生效）
/// SUBMITTED      → ACTIVE      审批通过（同槽旧版本 → SUPERSEDED，**同一事务**）
/// ACTIVE         → SUPERSEDED  被新版本取代（调整通过）
/// ACTIVE         → CLOSED      年度结账（只在年结时批量关）
/// DRAFT          → CANCELLED   草稿作废
/// SUBMITTED      → CANCELLED   审批中作废（先终止审批）
/// ```
///
/// **没有 `ACTIVE → DRAFT`**：已生效的预算不能退回草稿改，只能再调一版（ARCH-6 硬规则 4）。
pub const BUDGET_EDGES: [(&str, &str); 7] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "DRAFT"),
    ("SUBMITTED", "ACTIVE"),
    ("ACTIVE", "SUPERSEDED"),
    ("ACTIVE", "CLOSED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "CANCELLED"),
];

/// 需要 reason 的预算边：驳回 / 审批中作废 / 被取代（都要能追溯到为什么）。
pub const BUDGET_REASON_EDGES: [(&str, &str); 3] = [
    ("SUBMITTED", "DRAFT"),
    ("SUBMITTED", "CANCELLED"),
    ("ACTIVE", "SUPERSEDED"),
];

/// 预算的「不可改额度」状态：审批中与已生效都不许改 `total_amount`
/// （草稿可改；调整走新版本）。
pub const BUDGET_LOCKED: [&str; 4] = ["SUBMITTED", "ACTIVE", "SUPERSEDED", "CLOSED"];

// ==================== FIN-2 固定资产（ARCH-6 第五章） ====================

/// 实体类型：固定资产卡片（`biz_fixed_asset`，ARCH-6 §5.6）。
pub const ENTITY_FIXED_ASSET: &str = DocKind::FixedAsset.as_str();

/// 固定资产合法边（ARCH-6 §5.6 / `ck_biz_fa_status` 的四个状态）。
///
/// ```text
/// IN_USE   → IDLE       停用（在用但闲置，照提折旧）
/// IDLE     → IN_USE     启用（重新投用）
/// IN_USE   → DISPOSED   处置（出售 / 报废 / 盘亏，**审批通过后**才走这条）
/// IDLE     → DISPOSED   处置（闲置资产同样要清账）
/// DISPOSED → IN_USE     作废处置（红冲处置凭证 → 资产恢复在役）
/// ```
///
/// **没有 `DISPOSED → IDLE`**：资产从「已处置」回来只有一条路（作废处置），
/// 回来时的状态就是它在役时的状态（在役）；要闲置再走一次停用，留两条痕迹。
///
/// **没有 `→ CANCELLED`**：`CANCELLED` 是建卡后未启用即作废的口径，
/// 本卡不提供「建卡即作废」入口（建错了直接软删，`deleted_at` 才是它的语义）。
pub const FIXED_ASSET_EDGES: [(&str, &str); 5] = [
    ("IN_USE", "IDLE"),
    ("IDLE", "IN_USE"),
    ("IN_USE", "DISPOSED"),
    ("IDLE", "DISPOSED"),
    ("DISPOSED", "IN_USE"),
];

/// 需要 reason 的资产边：**五条全要**。
///
/// 理由：`biz_fixed_asset_change.reason` 是 `NOT NULL`（内控留痕），
/// 而每条状态边都必然写一行变动记录 ⇒ 边不带原因就写不出留痕行。
pub const FIXED_ASSET_REASON_EDGES: [(&str, &str); 5] = [
    ("IN_USE", "IDLE"),
    ("IDLE", "IN_USE"),
    ("IN_USE", "DISPOSED"),
    ("IDLE", "DISPOSED"),
    ("DISPOSED", "IN_USE"),
];

/// 资产「不可改」状态：已处置的卡片不许改金额 / 部门 / 使用人
/// （要改只能先作废处置）。
pub const FIXED_ASSET_LOCKED: [&str; 1] = ["DISPOSED"];

/// 实体类型：折旧批次（`biz_depreciation_batch`，ARCH-6 §5.5.5）。
pub const ENTITY_DEPRECIATION: &str = DocKind::Depreciation.as_str();

/// 折旧批次合法边（ARCH-6 §5.5.5 的状态图 + 「红冲后重跑复用同一批次行」）。
///
/// ```text
/// CALCULATED → POSTED      生成凭证 + 回写卡片
/// POSTED     → REVERSED    红冲（红字凭证 + 回滚卡片 + 明细标 REVERSED）
/// CALCULATED → SKIPPED     跳过（本期无应提折旧）
/// REVERSED   → CALCULATED  红冲后重跑（**复用同一行**，否则 uk(period_code) 挡死）
/// ```
///
/// `REVERSED → CALCULATED` 这条边是设计 §5.5.5 末尾点名必须有的：
/// 「红冲后不删批次，重跑复用同一批次行」。没有它，第一次重跑就是 500。
pub const DEPRECIATION_EDGES: [(&str, &str); 4] = [
    ("CALCULATED", "POSTED"),
    ("POSTED", "REVERSED"),
    ("CALCULATED", "SKIPPED"),
    ("REVERSED", "CALCULATED"),
];

/// 需要 reason 的折旧边：红冲与重跑都要写清为什么。
pub const DEPRECIATION_REASON_EDGES: [(&str, &str); 2] = [("POSTED", "REVERSED"), ("REVERSED", "CALCULATED")];

/// 折旧「不可改」状态：已过账 / 已红冲的批次行不许再改金额
/// （重跑走 `REVERSED → CALCULATED` 显式流转）。
pub const DEPRECIATION_LOCKED: [&str; 2] = ["POSTED", "REVERSED"];

// ==================== FIN-3 费用报销（ARCH-6 第四章） ====================

/// 实体类型：费用报销单（`biz_expense_report`，ARCH-6 §4.3）。
pub const ENTITY_EXPENSE: &str = DocKind::ExpenseReport.as_str();

/// 报销单合法边（ARCH-6 §4.3.2 逐条一致）。
///
/// ```text
/// DRAFT     → SUBMITTED   提交（预算守卫 + 发起审批 + usage OCCUPIED）
/// SUBMITTED → DRAFT       驳回 / 撤回（usage RELEASED）
/// SUBMITTED → APPROVED    审批通过（生成计提凭证 + usage CONSUMED，**同事务**）
/// APPROVED  → PAID        付清（付款事务里同时改 paid/unpaid/pay_status）
/// DRAFT     → CANCELLED   草稿作废（无副作用）
/// SUBMITTED → CANCELLED   审批中作废（先终止审批，再作废）
/// APPROVED  → CANCELLED   作废已入账（红冲凭证 + usage RELEASED + 状态，**同事务**）
/// ```
///
/// **没有 `PAID → *`**：钱已经付给员工了，要退钱是「员工退款」业务（v2），不是作废。
/// **没有 `APPROVED → APPROVED` 自环**：部分付款的状态变化只有 `pay_status`，
/// 状态没变就不该写状态日志（用 `transition_if_changed`，与 `payable_payment` 同款）。
pub const EXPENSE_EDGES: [(&str, &str); 7] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "DRAFT"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "PAID"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
];

/// 需要 reason 的报销边：驳回 / 审批中作废 / 作废已入账（都要能追溯到为什么）。
pub const EXPENSE_REASON_EDGES: [(&str, &str); 3] = [
    ("SUBMITTED", "DRAFT"),
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
];

/// 报销「不可改内容」的状态：提交之后明细 / 金额一律冻结
/// （要改只能驳回或撤回回草稿；已入账的单据改动等于篡改凭证依据）。
pub const EXPENSE_LOCKED: [&str; 4] = ["SUBMITTED", "APPROVED", "PAID", "CANCELLED"];

/// 报销单的 BASE_EDGES（与 [`EXPENSE_EDGES`] 逐条对应，顺序一致）。
pub const EXPENSE_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交审批（预算守卫）", false),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "驳回 / 撤回（打回草稿）", true),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过并入账", false),
    BaseEdge::mandatory("APPROVED", "PAID", "付款付清", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "审批中作废（先终止审批）", true),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "作废已入账（红冲凭证）", true),
];

/// 收货入库单合法边（与 `stockin_model::LEGAL_EDGES` 同源）。
///
/// P16 起多出 3 条**作废审批**边（六本账反向操作必须先复核）：
/// ```text
/// POSTED          → PENDING_CANCEL   发起作废审批（只挂审批单，账不动）
/// PENDING_CANCEL  → CANCELLED        审批通过 → 才执行 P9 的六本账回滚
/// PENDING_CANCEL  → POSTED           驳回 / 申请人撤回 → 回到已过账，账一点没动
/// ```
///
/// P-FLOW-3A 起再多 4 条**过账审批**边（入库链：先审后过账）：
/// ```text
/// DRAFT          → PENDING_POST   提交过账审批（只挂审批单，库存一分没加）
/// PENDING_POST   → POSTED         审批通过 → 才执行 post_in_tx（六本账一起动）
/// PENDING_POST   → DRAFT          驳回 / 撤回 → 打回草稿，账一点没动
/// PENDING_POST   → CANCELLED      审批中作废（撤审批单 + 单据作废，从未入过账所以不冲账）
/// ```
pub const STOCK_IN_EDGES: [(&str, &str); 10] = [
    ("DRAFT", "POSTED"),
    ("DRAFT", "CANCELLED"),
    ("POSTED", "CANCELLED"),
    ("POSTED", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
    ("PENDING_CANCEL", "POSTED"),
    ("DRAFT", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("PENDING_POST", "DRAFT"),
    ("PENDING_POST", "CANCELLED"),
];

/// 采购订单合法边（P10 · 卡面 §三 的 8 条 + 收货白名单必需的 2 条）。
///
/// 相对 P2-A2 的 4 条补了 `SUBMITTED→APPROVED` / `APPROVED→PARTIAL_RECEIVED` /
/// `APPROVED→RECEIVED` / `PARTIAL_RECEIVED→RECEIVED`。
/// **故意不补自环** `PARTIAL_RECEIVED→PARTIAL_RECEIVED`：继续部分收货不是状态变更，
/// 由调用方判「状态没变就不调 transition」，只写收货流水（卡面 §三 的 ⚠️）。
///
/// # 为什么比卡面的 8 条多两条（`SUBMITTED→*_RECEIVED`）
///
/// 开工实测：`APPROVED` **没有任何写入方** —— `purchase_dto::TRANSITION_TARGETS`
/// 只放行 `SUBMITTED / CLOSED / CANCELLED`，审批接入也没接采购单，而全库现存采购单
/// 的初始提交态就是 `SUBMITTED`（P2-B1 的收货白名单 `purchase_receivable` 本来就收
/// `SUBMITTED`）。若严格只登记卡面的 8 条，**所有真实采购单收货都会 409**
/// —— 那不是「收紧」而是把功能打断（P9/P2-B1 的收货链路当场不可用）。
/// 所以这两条边是**把既有生产行为登记进状态机**，不是为过测试放宽；`DRAFT → RECEIVED`
/// 依旧非法（单测 ⑯ 钉死）。
///
/// # D4 追加的第 11 条（`RECEIVED → PARTIAL_RECEIVED`）
///
/// 收货单可以作废（`POSTED → CANCELLED`，P9/P16 的既有能力）。作废之后：
/// 库存 / 成本六本账回滚，采购明细的「已收数量」随之回退（P2-3：它是派生值，
/// 本单变 `CANCELLED` 后就不再计入已过账集合）—— 此时采购单如果还是
/// `RECEIVED`，就变成「已收货却没有一分已收数量」，而且 `RECEIVED` 不在收货白名单里，
/// **老板再也收不进这批货**（D4 要解决的正是"收不了货"）。
///
/// 所以补这条回退边：**只在收货单作废的回退路径上使用**（`sync_purchase_status`
/// 的 `reason` 写明是哪张收货单作废触发的）。正向的 `DRAFT → RECEIVED` 依旧非法。
/// # R-P1-5 追加的第 12、13 条（`PARTIAL_RECEIVED → CANCELLED` / `RECEIVED → CANCELLED`）
///
/// 「作废一张已收货的采购单」在联动作废之前是**做不到**的：下游收货单作废把单据推回
/// `PARTIAL_RECEIVED`（`sync_purchase_status` 的回退方向），而旧边表里
/// `PARTIAL_RECEIVED` / `RECEIVED` 没有任何出边 → 用户只能看着一张废单挂着。
///
/// 联动作废（`erp::cancel_cascade`）的执行顺序是「先作废全部下游收货单 → 再作废采购单」，
/// 所以走到这一步时下游三本账已经回到基线、已收数量已经回退（派生口径自动生效），
/// 缺的只是这两条**带必填原因**的边。生产上它们只由联动作废使用
/// （`PURCHASE_ORDER_REASON_EDGES` 同步登记，原因写进 `biz_status_log`）。
pub const PURCHASE_ORDER_EDGES: [(&str, &str); 15] = [
    ("DRAFT", "SUBMITTED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "APPROVED"),
    ("SUBMITTED", "CLOSED"),
    ("SUBMITTED", "CANCELLED"),
    ("SUBMITTED", "PARTIAL_RECEIVED"),
    ("SUBMITTED", "RECEIVED"),
    // P-FLOW-3A：审批驳回 → 打回草稿（改完可重新提交，`DRAFT → SUBMITTED` 已在上方）
    ("SUBMITTED", "DRAFT"),
    ("APPROVED", "PARTIAL_RECEIVED"),
    ("APPROVED", "RECEIVED"),
    // P-FLOW-3A：审批通过后取消（`PURCHASE_ORDER_REASON_EDGES` 早就登记了这条，缺边=真流转 409）
    ("APPROVED", "CANCELLED"),
    ("PARTIAL_RECEIVED", "RECEIVED"),
    ("RECEIVED", "PARTIAL_RECEIVED"),
    // R-P1-5：联动作废（先作废全部下游收货单，再废本单）—— 见上方注释
    ("PARTIAL_RECEIVED", "CANCELLED"),
    ("RECEIVED", "CANCELLED"),
];

/// 库存调拨单合法边（与 `transfer_model::LEGAL_EDGES` 同源，P5 的 6 条）。
pub const TRANSFER_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "PENDING"),
    ("PENDING", "APPROVED"),
    ("PENDING", "CANCELLED"),
    ("APPROVED", "IN_TRANSIT"),
    ("IN_TRANSIT", "DONE"),
    ("DRAFT", "CANCELLED"),
];

/// 销售订单合法边（与 `sales_order_model::LEGAL_EDGES` 同源，销售链 · SO-05）。
///
/// ```text
/// DRAFT         → SUBMITTED       提交（同时把「明细 / 客户冻结 / 信用额度」三道守卫跑一遍）
/// SUBMITTED     → APPROVED        审批通过（可拉出库单）
/// APPROVED      → PARTIAL_OUT     部分出库（由出库过账回写，不是人工点出来的）
/// APPROVED      → OUTBOUND        一次发完（同上）
/// PARTIAL_OUT   → OUTBOUND        累计发完（同上）
/// APPROVED      → CLOSED          未发货直接关闭（客户不要了：剩余量作废）
/// PARTIAL_OUT   → CLOSED          剩余未发数量作废后关闭
/// DRAFT         → CANCELLED       草稿作废（从未占用过任何额度 / 库存）
/// SUBMITTED     → CANCELLED       提交后作废（必须给原因）
/// APPROVED      → CANCELLED       审批后作废（必须给原因）
/// ```
///
/// # 为什么状态域是 `PARTIAL_OUT` / `OUTBOUND` 而不是 `SHIPPED` 系
///
/// PRD §4.13.3 的状态流转图与 §4.13.4 算例**全文一致**用 `PARTIAL_OUT` / `OUTBOUND`；
/// `docs/ERP-批次2-技术架构.md` §2.2.1 的 `PARTIAL_SHIPPED` / `SHIPPED` 是旧稿且与 PRD 矛盾。
/// 库侧 `ck_sales_order_status` 只认 7 个 PRD 值（V0264 自检把 `SHIPPED` 当越界值断言被拒），
/// 所以本表必须与 `sales_model::STATUSES` 逐字一致，否则迁移能过的值在这里会被判非法边。
///
/// # 没有的边
///
/// - **只有一条 `* → DRAFT`**：`SUBMITTED → DRAFT` —— 审批驳回 / 撤回打回草稿
///   （SO-06 审批流；写这条边的只有审批回调 `sales_approval_bridge::on_so_settled`，
///   人工 `PATCH /status` 在 handler 里被显式拒绝）。`APPROVED → DRAFT` 仍然没有：
///   已审批的单据不能被打回草稿（否则已生效的承诺被悄悄撤销）。
/// - **没有 `OUTBOUND → *` / `CLOSED → *` / `CANCELLED → *`**：三个终态，出了就不再动
///   （出库后发现要退，走退货单，不是把已出库的销售订单改回去）。
/// - **没有 `SUBMITTED → APPROVED` 以外的人工审批结果**：`SUBMITTED → APPROVED` 只由审批
///   回调写（人工 `PATCH /status` 在 handler 里 400，指向审批中心）。
pub const SALES_ORDER_EDGES: [(&str, &str); 11] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "PARTIAL_OUT"),
    ("APPROVED", "OUTBOUND"),
    ("PARTIAL_OUT", "OUTBOUND"),
    ("APPROVED", "CLOSED"),
    ("PARTIAL_OUT", "CLOSED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
    // SO-06：审批驳回 / 撤回 → 打回草稿（改完可重新提交；审批意见写 remark）
    ("SUBMITTED", "DRAFT"),
];

/// 发货单合法边（ARCH-7 §4.2，4 条，唯一真相）。
///
/// ```text
/// DRAFT          → PENDING_POST   提交待过账（只改状态，库存一分没动）
/// PENDING_POST   → POSTED         过账（SO-10：生成销售出库单 + 扣库存 + 回写 delivered_qty）
/// DRAFT          → CANCELLED      草稿作废（从未占用过库存，不要原因）
/// PENDING_POST   → CANCELLED      待过账作废（释放未发量占用，**要原因**）
/// ```
///
/// **故意没有的边**（每条都是一个真问题）：
/// - `POSTED → CANCELLED`：已过账的货已经出库、`delivered_qty` 已累加，
///   作废要连带红冲凭证 / 回滚三本账（SO-10 的「已过账作废」卡），
///   不能靠一条状态边假装账回来了；
/// - `PENDING_POST → DRAFT`：提交后退回只发生在审批场景，发货单没有审批，
///   需要改就先作废重建（少一条边就少一处「账没动但状态乱了」的窗口）；
/// - `CANCELLED / POSTED → *`：两个终态，出了就不再动。
pub const SHIPMENT_EDGES: [(&str, &str); 4] = [
    ("DRAFT", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("DRAFT", "CANCELLED"),
    ("PENDING_POST", "CANCELLED"),
];

/// 收款单合法边（ARCH-7 §4.4，3 条，唯一真相）。
///
/// ```text
/// DRAFT     → CONFIRMED   确认收款（金额 / 客户 / 方式核对过，进入可核销状态）
/// CONFIRMED → POSTED      过账（生成收款凭证 借 1002 银行存款 / 贷 1122 应收账款）
/// DRAFT     → CANCELLED   草稿作废（必须给原因；已核销的收款库层 CHECK 也会拒）
/// ```
///
/// **故意没有的边**：
/// - `CONFIRMED → CANCELLED`：确认过的收款已经在「可核销」承诺里，作废要走
///   「反核销 → 回 DRAFT」或者后续的收款红冲单，不能一步抹掉（路线图里红冲属发票/红字单卡）；
/// - `POSTED → *`：过账即生成凭证，退回必须红冲凭证（`voucher::reverse_posted`），
///   不是状态机的一条边（与应付 `SETTLED` 之后同理）；
/// - `* → DRAFT`：没有任何边回到草稿（回归语义会让「已确认」的审计链断掉）。
pub const RECEIPT_EDGES: [(&str, &str); 3] = [("DRAFT", "CONFIRMED"), ("CONFIRMED", "POSTED"), ("DRAFT", "CANCELLED")];

/// 销售发票合法边（ARCH-7 §4.5，4 条，唯一真相）。
///
/// ```text
/// DRAFT  → ISSUED   开票（生成收入凭证 借 1122 / 贷 5001 + 2221.02，回写应收发票号）
/// ISSUED → POSTED   归档（发票已交付 / 已报税留档，不再产生凭证，终态）
/// DRAFT  → VOID     草稿作废（账上一分没动 ⇒ 没有可红冲的凭证，只作废单据）
/// ISSUED → VOID     开票后作废（红冲收入凭证，并撤回应收的发票号回写）
/// ```
///
/// **故意没有的边**：
/// - `POSTED → VOID`：归档即会计档案，稿面 §4.5 只给 `DRAFT / ISSUED → VOID`；
///   要改已归档的发票得走红字发票流程（ARCH-7 §15.2.3），不是原地作废；
/// - `* → DRAFT`：没有任何边回到草稿（发票号一旦开出就是审计线索）。
pub const INVOICE_EDGES: [(&str, &str); 4] = [
    ("DRAFT", "ISSUED"),
    ("ISSUED", "POSTED"),
    ("DRAFT", "VOID"),
    ("ISSUED", "VOID"),
];

/// 销售退货单合法边（ARCH-7 §15.2.4，7 条，唯一真相）。
///
/// ```text
/// DRAFT         → SUBMITTED      提交（草稿只改状态，账上一分没动）
/// SUBMITTED     → APPROVED       审批通过（本卡无审批流，人工推进；审批接入是后续卡）
/// APPROVED      → PENDING_POST   批准后进入待过账（过账前的最后一次确认）
/// PENDING_POST  → POSTED         过账（SO-30：回库 + 冲应收 + 红字凭证，同一事务）
/// DRAFT         → CANCELLED      草稿作废（从未占过可退量，不要原因）
/// SUBMITTED     → CANCELLED      提交后作废（**要原因**）
/// APPROVED      → CANCELLED      批准后作废（**要原因**）
/// ```
///
/// **故意没有的边**（每条都是一个真问题）：
/// - `PENDING_POST → CANCELLED`：待过账已经把「这批货要退回来」的承诺传下去了，
///   撤回要先回 `APPROVED` —— 稿面 §15.2.4 也只画到 `APPROVED` 的作废箭头；
/// - `POSTED → CANCELLED`：过账即已回库 / 已冲应收 / 已开红字，撤销必须走红冲链，
///   不是一条状态边；
/// - `* → DRAFT`：退货单没有「打回草稿」的边（本卡也没有改单接口），
///   要改就作废重建，少一条边就少一处「账没动但状态乱了」的窗口。
pub const SALES_RETURN_EDGES: [(&str, &str); 7] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
];

/// 采购退货单合法边（P2-03，6 条，唯一真相）。
///
/// ```text
/// DRAFT      → SUBMITTED    提交审批（只改状态，账上一分没动）
/// SUBMITTED  → APPROVED     审批通过（prt_approve 两级；此后才能确认退货）
/// APPROVED   → CONFIRMED    确认退货（出库回冲 + 应付红字 + 凭证，同一事务）
/// DRAFT      → CANCELLED    草稿作废（从未占过可退量，不要原因）
/// SUBMITTED  → CANCELLED    审批中作废（**要原因**）
/// APPROVED   → CANCELLED    已批准作废（**要原因**：财务已按它准备冲账）
/// ```
///
/// **故意没有的边**（每条都是一个真问题）：
/// - `SUBMITTED → DRAFT`：没有「打回草稿」的边 —— 驳回就是回草稿，但那走的是
///   审批回调（`mark_back_to_draft`），不是用户点出来的状态机边；把它登记成边会让
///   用户可以在审批中自己把单据抽回草稿（审批对象与执行对象不一致）；
/// - `CONFIRMED → *`：确认即已出库 / 已冲应付 / 已记账，撤销必须走红冲链，
///   不是一条状态边；
/// - `APPROVED → SUBMITTED`：批准之后只能往前（确认或作废）。
pub const PURCHASE_RETURN_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "CONFIRMED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
];

/// 应收单合法边（ARCH-7 §4.3 + SO-14 遗留卡，6 条，唯一真相）。
///
/// ```text
/// OPEN    → PARTIAL      部分收款（收款核销累加 received_amount）
/// OPEN    → SETTLED      一次收清
/// PARTIAL → SETTLED      收清尾款
/// OPEN    → WRITTEN_OFF  坏账核销（**唯一入口**：PATCH /receivable/{id}/write-off，必须给原因）
/// PARTIAL → WRITTEN_OFF  部分收回后余额核销为坏账
/// OPEN    → CANCELLED    来源出库单作废 ⇒ 应收冲销（**只从销售出库作废事务里走**，必须给原因）
/// ```
///
/// **与 [`PAYABLE_EDGES`] 的三处刻意不同**（照抄 payable 会撞库层 CHECK 23514）：
/// - payable 的 `PARTIAL_PAID` 在这里叫 `PARTIAL`（`ck_biz_ar_status` 只认后者）；
/// - payable 的作废审批两条边（`PENDING_CANCEL`）本卡不引入：应收的终止动作
///   （坏账 / 出库作废冲销）都直接终态，复核动作走 `biz_status_log` 留痕；
/// - payable 的 `CANCELLED` 在应收里**不是**「随便可以作废」：只有 `OPEN`（一分没收）
///   有这条边，语义是「来源出库单已作废 ⇒ 这笔销售从未发生」（V0290）。
///   已部分/全部收款的应收**没有**作废边 —— 钱已经收了，要冲回必须走收款退款单。
///
/// **故意没有的边**：`SETTLED → *` / `WRITTEN_OFF → *` / `CANCELLED → *`（三个终态）；
/// `SETTLED → WRITTEN_OFF`（已收清没有余额可核销 —— 真要冲回得走收款红冲，不是坏账）；
/// `PARTIAL → CANCELLED`（已收过钱 ⇒ 出库作废要先冲收款，handler 给 409）。
pub const RECEIVABLE_EDGES: [(&str, &str); 9] = [
    ("OPEN", "PARTIAL"),
    ("OPEN", "SETTLED"),
    ("PARTIAL", "SETTLED"),
    ("OPEN", "WRITTEN_OFF"),
    ("PARTIAL", "WRITTEN_OFF"),
    ("OPEN", "CANCELLED"),
    // ---- 收款退款单的反向边（SO-14 遗留卡 t_573bfe5b，V0294）----
    //
    // 收款的核销把应收往前推（`OPEN → PARTIAL → SETTLED`），退款必须能把它推回来：
    // 退款核销 `received_amount -= 冲减额` 之后，状态若停在 `SETTLED`，账面就是
    // 「状态说收清、金额说没收」—— 与 `ck_biz_ar_unsettled (unsettled = amount - received)`
    // 恒等式直接矛盾，且出库作废（要求 `status='OPEN' AND received_amount=0`）永远走不通。
    //
    // 这三条**不改变既有语义**：`PARTIAL/SETTLED → CANCELLED` 仍然没有边
    // （已收过钱的应收不能靠作废抹掉，必须先退款），CANCELLED 依旧是「一分没收」。
    ("SETTLED", "PARTIAL"),
    ("PARTIAL", "OPEN"),
    ("SETTLED", "OPEN"),
];

/// 收款退款单合法边（3 条，唯一真相）。
///
/// ```text
/// DRAFT     → CONFIRMED   确认退款（金额 / 客户 / 核销对象核对过）
/// CONFIRMED → POSTED      过账（生成退款凭证 借 1122 应收账款 / 贷 1002 银行存款）
/// DRAFT     → CANCELLED   草稿作废（必须给原因；已核销的退款库层 CHECK 也会拒）
/// ```
///
/// **故意没有 `POSTED → *`**：钱已付出且凭证已记账，退款单不能作废 ——
/// 要抹掉只能由客户重新回一张收款单（钱又回来了），或在凭证层红冲
/// （`voucher::reverse_posted`）。与收款单「`CONFIRMED → CANCELLED` 也没有边」同一套理由。
pub const RECEIPT_REFUND_EDGES: [(&str, &str); 3] =
    [("DRAFT", "CONFIRMED"), ("CONFIRMED", "POSTED"), ("DRAFT", "CANCELLED")];

/// 应付单合法边（P7 的付款累计 + 作废；P-FLOW-3B 加 3 条作废审批边）。
///
/// P-FLOW-3B 起多出 3 条**作废审批**边（改账动作必须先复核）：
/// ```text
/// OPEN            → PENDING_CANCEL   发起作废审批（只挂审批单，欠款还在）
/// PENDING_CANCEL  → CANCELLED        审批通过 → 才真正作废（进项凭证红冲 + 欠款消失）
/// PENDING_CANCEL  → OPEN             驳回 / 撤回 → 回到未付，账一点没动
/// ```
///
/// **故意没有** `PARTIAL_PAID / SETTLED → PENDING_CANCEL`（卡面决策）：
/// 作废一张已经付过钱的应付单必须同时冲回已付款，那是「付款冲销 + 红冲凭证」的活，
/// 不是作废单据能表达的。入口处还会再核一次 `paid_amount = 0`（双保险）。
pub const PAYABLE_EDGES: [(&str, &str); 7] = [
    ("OPEN", "PARTIAL_PAID"),
    ("OPEN", "SETTLED"),
    ("PARTIAL_PAID", "SETTLED"),
    ("OPEN", "CANCELLED"),
    ("OPEN", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
    ("PENDING_CANCEL", "OPEN"),
];

/// 凭证的合法边（P0-FIX-4A 起）：`DRAFT → AUDITED → POSTED` 双人复核链。
///
/// ```text
/// DRAFT    → AUDITED   审核（制单人以外的账号；内控闸在入口，403 + 65019）
/// AUDITED  → DRAFT     反审核（**必须给原因**，写进 biz_status_log）
/// AUDITED  → POSTED    记账（此后进总账 / 报表；之后只能红冲，不能反审核）
/// ```
///
/// # 为什么 `POSTED` 没有出边（刻意）
///
/// 已记账凭证的更正语义是「红冲开新单」，不是「把状态改回去」：一旦允许
/// `POSTED → DRAFT`，总账 / 余额表 / 四大报表（`WHERE v.status = 'POSTED'`）就会
/// 少掉一笔**已经对外出过报表**的金额 —— 报表口径会被一次点错改写。
/// 所以 `POSTED` 在本表里是终态，退出只能是红字凭证。
///
/// 同理 **也没有** `DRAFT → POSTED`：制单与审核必须分离（P0-FIX-4A 的内控目标），
/// 少了这条捷径，前端少画一个按钮的同时后端也真的没有这条路。
///
/// # 与「凭证红冲审批」的关系（P-FLOW-3B）
///
/// 红冲审批**不进**本表：红冲的语义是「生成红字凭证、原凭证保持 `POSTED`」，
/// 若真把原凭证改成 `PENDING_REVERSE`，报表取数会让这张已入账的凭证消失 ——
/// 审批流程不该改变账务的存在性。「审批中」用 `biz_voucher.pending_reverse_instance_id`
/// 表达，与本表正交。
///
/// # 自动凭证
///
/// 收货 / 付款 / 盘盈亏 / 年结 / 红冲产生的凭证是**系统记账**（`post_voucher` 直接落
/// `POSTED`），不是「某个人制的单」，不存在双人复核的前提；它们的复核由上游单据
/// （过账审批 / 付款审批 / 作废审批）承担。
pub const VOUCHER_EDGES: [(&str, &str); 3] = [("DRAFT", "AUDITED"), ("AUDITED", "DRAFT"), ("AUDITED", "POSTED")];

/// 会计期间的合法边（ERP-P1）。
///
/// ```text
/// NOT_OPENED → OPEN    开账（期间行已存在但还没启用）
/// OPEN       → CLOSED  结账（四项前置校验全过才允许）
/// CLOSED     → OPEN    反结账（必填原因，且后续期间不能有已结账的）
/// ```
///
/// **没有** `CLOSED → CLOSED` / `OPEN → OPEN`：重复结账 / 重复开账不是状态变更，
/// 由调用方在入口给出 409 文案（「已开账」/「已结账」），不走状态机。
pub const PERIOD_EDGES: [(&str, &str); 3] = [("NOT_OPENED", "OPEN"), ("OPEN", "CLOSED"), ("CLOSED", "OPEN")];

/// 需要 reason 的期间边：只有反结账（撤销已封账的月份，必须留原因）。
///
/// 结账的 `reason` 是**可选**备注（卡面 §一 ②），不进本清单 ——
/// 进了就会被 `transition` 挡成 400「必须给原因」，与卡面冲突。
pub const PERIOD_REASON_EDGES: [(&str, &str); 1] = [("CLOSED", "OPEN")];

/// 期间没有「入账态只读」的概念：`CLOSED` 的只读性由
/// [`crate::business::erp::period::ensure_period_writable`] 在记账入口拦截（400），
/// 不走 `ensure_writable`（那是「单据字段不许改」的闸，语义不同）。
pub const PERIOD_LOCKED: [&str; 0] = [];

/// 年结的合法边（ERP-R4）。
///
/// ```text
/// NOT_CLOSED → CLOSED      年结（五项前置校验全过才允许）
/// CLOSED     → NOT_CLOSED  反年结（必填原因 ≥ 10 字，且下一年度不能有凭证）
/// ```
///
/// 与期间一样**没有自环**：重复年结 / 重复反年结不是状态变更，入口给 409。
pub const YEAR_END_EDGES: [(&str, &str); 2] = [("NOT_CLOSED", "CLOSED"), ("CLOSED", "NOT_CLOSED")];

/// 需要 reason 的年结边：反年结（把已结转的年度拆回，必须留原因）。
/// 年结本身的 `reason` 是可选备注（与期间结账同口径）。
pub const YEAR_END_REASON_EDGES: [(&str, &str); 1] = [("CLOSED", "NOT_CLOSED")];

/// 年结没有「入账态只读」：结转凭证一旦生成就由 [`crate::business::erp::year_end`]
/// 反年结删掉，不存在「字段不许改」的单据形态。
pub const YEAR_END_LOCKED: [&str; 0] = [];

/// 需要 reason 的边（作废已入账单据：红冲必须能追溯到为什么）。
///
/// P16：作废拆成「发起（POSTED→PENDING_CANCEL）」和「执行（PENDING_CANCEL→CANCELLED）」两步，
/// 两步都要原因 —— 前者是**申请理由**（审批人据此判断），后者是**记账依据**（红冲凭证的摘要）。
pub const STOCK_IN_REASON_EDGES: [(&str, &str); 3] = [
    ("POSTED", "CANCELLED"),
    ("POSTED", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
];
/// 需要 reason 的边：已提交的采购单作废。
pub const PURCHASE_ORDER_REASON_EDGES: [(&str, &str); 4] = [
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
    // R-P1-5：联动作废走的两条边（作废已收货的单必须留原因）
    ("PARTIAL_RECEIVED", "CANCELLED"),
    ("RECEIVED", "CANCELLED"),
];
/// 需要 reason 的边：销售订单的作废与关闭。
///
/// - `SUBMITTED / APPROVED → CANCELLED`：已经占用信用额度（提交后进入在手订单口径）、
///   审批后可能已排产 / 备货，作废原因必须能追溯；
/// - `APPROVED / PARTIAL_OUT → CLOSED`：关闭 = **剩余未发数量永久作废**，
///   客户改主意 / 缺料停单都要留原因（对账时要能回答「这单为什么没发完」）。
///
/// `DRAFT → CANCELLED` 不要原因：草稿从未产生对外影响。
pub const SALES_ORDER_REASON_EDGES: [(&str, &str); 4] = [
    ("SUBMITTED", "CANCELLED"),
    ("APPROVED", "CANCELLED"),
    ("APPROVED", "CLOSED"),
    ("PARTIAL_OUT", "CLOSED"),
];
/// 需要 reason 的边：发货单「待过账作废」。
///
/// 只有 `PENDING_POST → CANCELLED` 要原因：这时单据已经占用了订单行的未发量
/// （别处已经按「这批要发」排了货 / 通知了客户），作废必须能回答「为什么」。
/// `DRAFT → CANCELLED` 不要原因 —— 草稿从未对外产生任何影响。
pub const SHIPMENT_REASON_EDGES: [(&str, &str); 1] = [("PENDING_POST", "CANCELLED")];
/// 需要 reason 的边：应收**坏账核销**与**出库作废冲销**。
///
/// 核销把一笔应收从「资产」变成「损失」（借方 6702 / 贷方 1122），是**改账**动作：
/// 审计要能回答「哪笔、为什么判为收不回来、谁批的」。所以入边强制原因
/// （缺失 → `transition` 直接 400），原因同时落 `biz_status_log.reason` 与单据 `remark`。
///
/// `OPEN → CANCELLED` 同样必须给原因：作废一张应收的理由来自**上游出库单的作废原因**
/// （`stockout_handler::cancel_stock_out` 强制 `reason`），冲销凭据要能追回那张单据。
///
/// 收款相关的三条边（`OPEN→PARTIAL` / `OPEN→SETTLED` / `PARTIAL→SETTLED`）**不要**原因：
/// 那是正常业务推进，依据是收款单本身。
pub const RECEIVABLE_REASON_EDGES: [(&str, &str); 3] = [
    ("OPEN", "WRITTEN_OFF"),
    ("PARTIAL", "WRITTEN_OFF"),
    ("OPEN", "CANCELLED"),
];
/// 需要 reason 的边：收款单**作废**（`DRAFT → CANCELLED`）。
///
/// 作废一张已登记的收款单等于宣布「这笔钱没收到 / 记错了」，是**改账**动作：
/// 审计要能回答「哪笔、为什么作废、谁批的」。`DRAFT → CONFIRMED` / `CONFIRMED → POSTED`
/// 是正常推进（依据是收款单本身），不要原因。
pub const RECEIPT_REASON_EDGES: [(&str, &str); 1] = [("DRAFT", "CANCELLED")];

/// 需要 reason 的边：收款退款单**作废**（`DRAFT → CANCELLED`）。
///
/// 退款是「钱往外走」的动作，作废一张已登记的退款单等于宣布「这笔钱退错了 / 不退了」，
/// 审计要能回答「哪笔、为什么」：`reason` 在**建单时就必填**（`ck_biz_receipt_refund_reason`），
/// 作废时再强制一次。
pub const RECEIPT_REFUND_REASON_EDGES: [(&str, &str); 1] = [("DRAFT", "CANCELLED")];
/// 需要 reason 的边：发票作废（`DRAFT → VOID` / `ISSUED → VOID`）。
///
/// 作废一张发票等于宣布「这张票不认了」：`ISSUED` 作废还要红冲收入凭证，
/// 审计必须能回答「哪张、为什么、谁批的」。开票（`DRAFT → ISSUED`）与归档
/// （`ISSUED → POSTED`）是正常推进，不要原因。
pub const INVOICE_REASON_EDGES: [(&str, &str); 2] = [("DRAFT", "VOID"), ("ISSUED", "VOID")];
/// 需要 reason 的退货单边：提交后 / 批准后作废。
///
/// 草稿作废不要原因（从未提交过、也没占到任何账务）；提交之后作废意味着
/// 「这单本来要退的货不退了」—— 审计要能回答为什么，所以强制原因。
pub const SALES_RETURN_REASON_EDGES: [(&str, &str); 2] = [("SUBMITTED", "CANCELLED"), ("APPROVED", "CANCELLED")];
/// 需要 reason 的采购退货单边：提交后 / 批准后作废。
///
/// 与销售退货同口径：草稿作废不要原因（从未提交、没占任何账务）；
/// 提交之后作废意味着「这单本来要退的货不退了」，审计要能回答为什么。
pub const PURCHASE_RETURN_REASON_EDGES: [(&str, &str); 2] = [("SUBMITTED", "CANCELLED"), ("APPROVED", "CANCELLED")];

/// 需要 reason 的边：调拨单取消。
pub const TRANSFER_REASON_EDGES: [(&str, &str); 2] = [("DRAFT", "CANCELLED"), ("PENDING", "CANCELLED")];
/// 需要 reason 的边：应付作废（会计上作废必须留痕）。
///
/// P-FLOW-3B：作废拆成「发起（OPEN → PENDING_CANCEL）」与「执行（PENDING_CANCEL → CANCELLED）」
/// 两步，两步都要原因 —— 前者是**申请理由**（财务经理据此判断该不该批），
/// 后者是**记账依据**（进项凭证红冲的摘要）。驳回那条边（PENDING_CANCEL → OPEN）不要原因：
/// 驳回意见由审批待办自己留痕，单据只是回位。
pub const PAYABLE_REASON_EDGES: [(&str, &str); 3] = [
    ("OPEN", "CANCELLED"),
    ("OPEN", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
];
/// 需要 reason 的凭证边：只有反审核（`AUDITED → DRAFT`）。
///
/// 反审核是**把已复核的凭证打回可编辑态**：审计要能回答「谁、什么时候、为什么」
/// 把一张已审核的凭证拆了封条，所以原因必填（缺失 → `transition` 直接 400）。
/// 审核（`DRAFT → AUDITED`）的 `remark` 是**可选**意见（前端 `{ remark? }`），
/// 所以不进本清单 —— 进了就会被状态机挡成 400「必须给原因」，与卡面契约冲突。
/// 记账（`AUDITED → POSTED`）是既定流程的下一步，无需原因。
pub const VOUCHER_REASON_EDGES: [(&str, &str); 1] = [("AUDITED", "DRAFT")];

/// 入账态（这些状态下不允许改业务字段，见 [`ensure_writable`]）。
///
/// `PENDING_CANCEL` 也锁：作废审批中如果还能改数量 / 金额 / 明细，审批人批的就是一份
/// 已经变过的数据（审批对象与执行对象不是同一张单），复核等于没做。
///
/// `PENDING_POST`（P-FLOW-3A）同理且更严格：过账审批批的是「这一版的数量与金额」，
/// 批完立刻按单据落库存 / 凭证 / 应付，中途改一行就是「审 A 记 B」。
pub const STOCK_IN_LOCKED: [&str; 3] = ["POSTED", "PENDING_CANCEL", "PENDING_POST"];
/// 采购订单入账态：收过货的单据不能再改金额 / 明细。
pub const PURCHASE_ORDER_LOCKED: [&str; 2] = ["RECEIVED", "PARTIAL_RECEIVED"];
/// 销售订单入账态：审批通过之后不能再改金额 / 明细。
///
/// `SUBMITTED` 不锁：审批中的单子在**改单入口**已被 `status = DRAFT` 拦死
/// （销售订单只有草稿能改，见 `sales_order_handler::update_sales_order`），
/// 这里列的是「一旦命中就绝不能再按普通改单写」的状态。
pub const SALES_ORDER_LOCKED: [&str; 5] = ["APPROVED", "PARTIAL_OUT", "OUTBOUND", "CLOSED", "CANCELLED"];
/// 发货单入账态：过账（库存已动 / 已回写 delivered_qty）与作废两个终态。
///
/// `PENDING_POST` **不在**入账态：提交待过账只是「排队」，改数量 / 改库位都没动过账，
/// 允许撤回式修改（先作废重建）比锁死更省事，但**不允许原地改** —— 见 handler。
pub const SHIPMENT_LOCKED: [&str; 2] = ["POSTED", "CANCELLED"];
/// 应收单入账态：已结清（钱收完了，金额不许再动）、已核销（坏账终态）与已作废（出库作废冲销）。
///
/// `PARTIAL` **不在**入账态：部分收款之后还要继续核销（`PARTIAL → SETTLED` 的
/// `received_amount` 会继续累加），锁死就没法收款了。
pub const RECEIVABLE_LOCKED: [&str; 3] = ["SETTLED", "WRITTEN_OFF", "CANCELLED"];
/// 收款单入账态：已过账（凭证已生成，金额 / 客户 / 核销都不许再动）与已作废。
///
/// `CONFIRMED` **不在**入账态：确认之后还要核销（`allocate` 要改 `allocated_amount`），
/// 锁死就没法核销了 —— 与应收的 `PARTIAL` 同理。
pub const RECEIPT_LOCKED: [&str; 2] = ["POSTED", "CANCELLED"];
/// 收款退款单入账态：已过账（凭证已生成，金额 / 核销都不许再动）与已作废。
///
/// `CONFIRMED` **不在**入账态：确认之后还要核销（要改 `allocated_amount`），
/// 锁死就没法核销了 —— 与收款单 / 应收的 `PARTIAL` 同理。
pub const RECEIPT_REFUND_LOCKED: [&str; 2] = ["POSTED", "CANCELLED"];
/// 发票入账态：已开票（凭证已生成、应收已回写）、已归档、已作废，金额 / 客户 / 税率都不许再动。
///
/// `DRAFT` **不在**入账态：草稿还要走开票流程（`ISSUED` 时才生成凭证）。
pub const INVOICE_LOCKED: [&str; 3] = ["ISSUED", "POSTED", "VOID"];
/// 退货单入账态：已过账（已回库 / 已冲应收 / 已开红字，明细不许再动）与已作废。
///
/// `PENDING_POST` **不在**入账态：待过账还能被 SO-30 的过账事务读取处理，
/// 锁死会让过账前的一切准备都落空（与发货单的 `PENDING_POST` 同口径）。
pub const SALES_RETURN_LOCKED: [&str; 2] = ["POSTED", "CANCELLED"];
/// 采购退货单入账态：已确认（账已按它动过，明细不许再改）与已作废。
pub const PURCHASE_RETURN_LOCKED: [&str; 2] = ["CONFIRMED", "CANCELLED"];
/// 调拨单入账态：审批通过即锁定（在途 / 完成都不可改）。
pub const TRANSFER_LOCKED: [&str; 3] = ["APPROVED", "IN_TRANSIT", "DONE"];
/// 应付入账态：已结清 / 已作废都锁定（作废的单子当然不能再改）。
///
/// P-FLOW-3B 把 `PENDING_CANCEL` 也加进来：作废审批中如果还能改金额 / 备注，
/// 财务经理批的就是一份已经变过的数据（审批对象 ≠ 执行对象），复核等于没做。
pub const PAYABLE_LOCKED: [&str; 3] = ["SETTLED", "CANCELLED", "PENDING_CANCEL"];
/// 凭证入账态：已记账 / 已审核。
///
/// `POSTED` 是会计档案（改了摘要就与已出报表不一致），`AUDITED` 是**复核过的版本**：
/// 审核之后如果还能改金额 / 分录 / 摘要，记账人记的就是另一份数据（审 A 记 B），
/// 复核等于没做。所以两者都锁；只有 `DRAFT` 可编辑（改完重新审核）。
pub const VOUCHER_LOCKED: [&str; 2] = ["POSTED", "AUDITED"];

// ==================== 合同（ARCH-2 · A1 基座） ====================

/// 合同的合法边（ARCH-2 §4.1 + ARCH-3 R-P1-1，7 条，唯一真相）。
///
/// ```text
/// DRAFT            → PENDING_APPROVAL  提交审批（发起 contract_approve 流程）
/// PENDING_APPROVAL → SIGNED            审批通过 → 已签订（三个日期必填，V-08；乙方存在 V-11）
/// PENDING_APPROVAL → DRAFT             驳回 / 撤回 / 终止审批 → 回位草稿，可改后重提
/// SIGNED           → ACTIVE            生效（到达 start_date 或手工确认）
/// SIGNED           → TERMINATED        已签未生效即终止（**必须给原因**，DEC-05）
/// ACTIVE           → COMPLETED         履约完成
/// ACTIVE           → TERMINATED        中途终止（**必须给原因**）
/// ```
///
/// # 为什么 A1 的 `DRAFT → SIGNED` 被换掉了（ARCH-3 §6 R-P1-1 · 断链 DC-02）
///
/// A1 的 5 条边是 `DRAFT → SIGNED → ACTIVE → COMPLETED/TERMINATED`。问题在于
/// **`DRAFT → SIGNED` 一步就把合同变成「可被采购单引用、可用于付钱」的已签状态，
/// 中间没有任何人审批** —— 采购单自己有 `po_approve` 复核，而它挂的合同是一人点一下
/// 就生效的，等于「给合同付钱」这件事只审了单、没审合同。
///
/// R-P1-1 的修法是插一个**待审批态**：`DRAFT → PENDING_APPROVAL → SIGNED`。
/// 于是：
///
/// * **没有任何边能一步从 `DRAFT` 到 `SIGNED`/`ACTIVE`** —— 「不审批直接点签订/生效」
///   在状态机层面就不成立（handler 给 409 + 66015，文案指向「先提交审批」）；
/// * `SIGNED` 只有一个入口（审批通过），`ACTIVE` 只有一个入口（`SIGNED → ACTIVE`），
///   所以「合同可用于采购单」= 「它一定被批过」；
/// * 撤回/驳回走 `PENDING_APPROVAL → DRAFT`（不是新增 `CANCELLED` 态：草稿作废仍走
///   软删除，见 §4.2 与 [`CONTRACT_LOCKED`] 下的说明）。
///
/// 状态**值**没变（`DRAFT / SIGNED / ACTIVE / COMPLETED / TERMINATED` 语义与顺序原样），
/// 只多了一个「审批中」的中间态 —— 与 ARCH-3「加一条 `DRAFT → 待审批` 过渡」的写法一致。
///
/// ⚠️ 与实现卡正文的差异：卡面写「DRAFT → ACTIVE / CANCELLED、ACTIVE → … / CANCELLED」，
/// 但 ARCH-2（以及建表 CHECK 的白名单）**没有 `CANCELLED` 态** —— 草稿作废走
/// **软删除**（§4.2：草稿从未对外生效，「终止」是对已签合同的动作，两者混用会让
/// 「终止率」统计失真）。卡面本身写了「按文档实际写的来」，故不加 `CANCELLED`。
///
/// 禁止边 + 自环见 §4.2，单测逐条覆盖（`contract_*` 测试组）。
pub const CONTRACT_EDGES: [(&str, &str); 7] = [
    ("DRAFT", "PENDING_APPROVAL"),
    ("PENDING_APPROVAL", "SIGNED"),
    ("PENDING_APPROVAL", "DRAFT"),
    ("SIGNED", "ACTIVE"),
    ("SIGNED", "TERMINATED"),
    ("ACTIVE", "COMPLETED"),
    ("ACTIVE", "TERMINATED"),
];

/// 合同的「审批中」状态（`approval_instance_id` 非空的唯一合法状态）。
///
/// 与 `biz_stock_in.PENDING_POST` / `biz_purchase_order.SUBMITTED` 同一语义：
/// 「单子已经交出去等人批，业务字段冻结，等审批结论推进」。
pub const CONTRACT_PENDING_APPROVAL: &str = "PENDING_APPROVAL";

/// 需要 reason 的合同边：两条终止边（§4.1 E3 / E5）。
///
/// 终止原因同时写 `biz_status_log.reason` 与 `biz_contract.remark`（V-18），
/// 不新增列（§2.1「为什么没有这些列」）。
pub const CONTRACT_REASON_EDGES: [(&str, &str); 2] = [("SIGNED", "TERMINATED"), ("ACTIVE", "TERMINATED")];

/// 合同的「非草稿」状态集合：`SIGNED` 之后关键字段冻结（§5.3）。
///
/// ⚠️ **用法必须收窄**（ARCH-2 §4.1 末段）：其它实体的 locked 是「入账态，整单只读」，
/// 合同则是「已签后只冻结关键字段，备注 / 负责人 / 部门 / 名称仍可改」。
/// 因此 `locked_statuses()` **只用于**前端置灰与 `is_locked` 判断；
/// 写库校验必须走 §5.3 的字段白名单（A2 的 `contract_handler::update` 读旧行比对），
/// **handler 不得直接调 `ensure_writable`** —— 否则连「终止原因写 remark」都会被自己挡住。
///
/// R-P1-1 把 `PENDING_APPROVAL` 也收进来（与收货单的 `PENDING_POST` 同口径）：
/// 审批中如果还能改金额 / 乙方 / 日期，审批人批的就是一份**已经变过的数据**
/// （审批对象 ≠ 执行对象），复核等于没做。真正的写库闸是 `contract_apply` 的
/// `WHERE status = 'DRAFT'`（非草稿一律改不动），本表只影响前端置灰与文案。
pub const CONTRACT_LOCKED: [&str; 5] = ["PENDING_APPROVAL", "SIGNED", "ACTIVE", "COMPLETED", "TERMINATED"];

// ==================== 第 1 层：BASE_EDGES（硬约束白名单） ====================
//
// 【D1 流程配置化 · 阶段 3】
//
// 边表分三层，**真相永远在代码里**：
//
// ```text
// 第 1 层（本段）BASE_EDGES   —— 每个实体能走的边 + 每条边能不能被配置禁用
// 第 2 层（表）biz_flow_state_edge —— 只能从第 1 层挑子集，且只能加严
// 第 3 层（运行时）effective_edges —— 两层合并；读失败一律退化回第 1 层
// ```
//
// 为什么「可选边」这么少：配置的合法用途是**加严**（要求走审批、要求填原因、
// 追加权限码）和**关掉捷径**（例如「采购单不收货就能直接关闭」）。把主链、审批落地、
// 撤销回位、红冲这些边禁掉不是加严而是**停摆**（单据卡死 / 审批回调 500 / 账不可纠正），
// 所以它们标 `mandatory`：配置禁用会被**忽略**并打 WARN，边照旧可用。
//
// 可选边的判据（写死在这里，评审可逐条核）：
// 这条边被关掉之后，同一 `from` 状态**还有别的路能走**，且关掉它等于要求走更严的那条路
// （必须经过审批 / 必须走作废审批而不是草稿作废）。运行时另有一道护栏：
// 合并后任何一个 `from` 状态**没有出边**就整份配置作废（退回 BASE_EDGES）。

/// 一条边的元信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeMeta {
    /// 配置层能不能禁用它（`true` = 永远启用；配置禁用会被忽略并打 WARN）。
    pub mandatory: bool,
    /// 默认显示文案（配置层可以用 `biz_flow_state_edge.label` 覆盖）。
    pub default_label: &'static str,
    /// 走这条边是否必须给原因（**配置层只能加，不能减**）。
    pub requires_reason: bool,
}

/// BASE_EDGES 的一条：`(from, to) + EdgeMeta`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseEdge {
    /// 变更前状态。
    pub from: &'static str,
    /// 变更后状态。
    pub to: &'static str,
    /// 元信息。
    pub meta: EdgeMeta,
}

impl BaseEdge {
    /// 硬约束边：配置不许禁用（主链 / 审批落地 / 撤销回位 / 红冲）。
    pub const fn mandatory(
        from: &'static str,
        to: &'static str,
        default_label: &'static str,
        requires_reason: bool,
    ) -> Self {
        Self {
            from,
            to,
            meta: EdgeMeta {
                mandatory: true,
                default_label,
                requires_reason,
            },
        }
    }

    /// 可选边（捷径 / 旁路）：配置可以禁用；禁用 = 必须走更严的那条路。
    pub const fn optional(
        from: &'static str,
        to: &'static str,
        default_label: &'static str,
        requires_reason: bool,
    ) -> Self {
        Self {
            from,
            to,
            meta: EdgeMeta {
                mandatory: false,
                default_label,
                requires_reason,
            },
        }
    }

    /// `(from, to)` 比较键。
    pub const fn key(&self) -> (&'static str, &'static str) {
        (self.from, self.to)
    }
}

/// 收货入库单的 BASE_EDGES（与 [`STOCK_IN_EDGES`] 逐条对应，顺序一致）。
///
/// 可选边只有两条「捷径」：`DRAFT → POSTED`（跳过过账审批）、
/// `PENDING_POST → CANCELLED`（审批中直接作废，本该先打回草稿）。
pub const STOCK_IN_BASE: [BaseEdge; 10] = [
    BaseEdge::optional("DRAFT", "POSTED", "直接过账", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("POSTED", "CANCELLED", "作废红冲", true),
    BaseEdge::mandatory("POSTED", "PENDING_CANCEL", "发起作废审批", true),
    BaseEdge::mandatory("PENDING_CANCEL", "CANCELLED", "审批通过作废", true),
    BaseEdge::mandatory("PENDING_CANCEL", "POSTED", "驳回回已过账", false),
    BaseEdge::mandatory("DRAFT", "PENDING_POST", "提交过账审批", false),
    BaseEdge::mandatory("PENDING_POST", "POSTED", "审批通过过账", false),
    BaseEdge::mandatory("PENDING_POST", "DRAFT", "打回草稿", false),
    BaseEdge::optional("PENDING_POST", "CANCELLED", "审批中作废", false),
];

/// 采购订单的 BASE_EDGES（与 [`PURCHASE_ORDER_EDGES`] 逐条对应，顺序一致）。
///
/// 可选边只有 `SUBMITTED → CLOSED`（**不收货直接关闭**，卡面点名的那个例子：
/// 「客户不想让采购单能直接关闭」）。
pub const PURCHASE_ORDER_BASE: [BaseEdge; 15] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::optional("SUBMITTED", "CLOSED", "直接关闭", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "作废", true),
    BaseEdge::mandatory("SUBMITTED", "PARTIAL_RECEIVED", "部分收货", false),
    BaseEdge::mandatory("SUBMITTED", "RECEIVED", "收货完成", false),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "审批驳回打回", false),
    BaseEdge::mandatory("APPROVED", "PARTIAL_RECEIVED", "部分收货", false),
    BaseEdge::mandatory("APPROVED", "RECEIVED", "收货完成", false),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "作废", true),
    BaseEdge::mandatory("PARTIAL_RECEIVED", "RECEIVED", "收货完成", false),
    BaseEdge::mandatory("RECEIVED", "PARTIAL_RECEIVED", "收货单作废回退", false),
    // R-P1-5：联动作废专用（下游已全部作废之后才走）—— 必须给原因
    BaseEdge::mandatory("PARTIAL_RECEIVED", "CANCELLED", "联动作废", true),
    BaseEdge::mandatory("RECEIVED", "CANCELLED", "联动作废", true),
];

/// 销售订单的 BASE_EDGES（与 [`SALES_ORDER_EDGES`] 逐条对应，顺序一致）。
///
/// 11 条边**全部 mandatory**：销售订单没有「能关掉的捷径」——
///   · `APPROVED → CLOSED` 看着像捷径，但关掉之后从 `APPROVED` 就只剩出库一条路，
///     而 `OUTBOUND` 是终态（没有 `OUTBOUND → CLOSED`）⇒ 关掉它等于把单据卡死；
///   · `DRAFT → CANCELLED` 关掉之后草稿单永远删不掉（本表只有 `SUBMITTED → DRAFT` 一条回草稿的边、
///     也没有物理删接口）。
/// 与 `TRANSFER_BASE`（六条全 mandatory）同口径。
///
/// 要原因的四条：提交后 / 审批后作废、两种「关闭」（关闭 = 剩余未发数量作废，必须留痕）。
/// `SUBMITTED → DRAFT` 不强制原因：审批驳回意见由回调写进 `remark` + `biz_status_log`。
pub const SALES_ORDER_BASE: [BaseEdge; 11] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("APPROVED", "PARTIAL_OUT", "部分出库", false),
    BaseEdge::mandatory("APPROVED", "OUTBOUND", "出库完成", false),
    BaseEdge::mandatory("PARTIAL_OUT", "OUTBOUND", "出库完成", false),
    BaseEdge::mandatory("APPROVED", "CLOSED", "关闭（剩余量作废）", true),
    BaseEdge::mandatory("PARTIAL_OUT", "CLOSED", "关闭（剩余量作废）", true),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "作废", true),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "作废", true),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "审批驳回打回草稿", false),
];

/// 发货单的 BASE_EDGES（与 [`SHIPMENT_EDGES`] 逐条对应，顺序一致）。
///
/// 四条边全 `mandatory = true`：主链（提交 → 过账）与两个作废口，
/// **没有可选边可关**（关掉「过账」等于让单据永远停在待过账）。
pub const SHIPMENT_BASE: [BaseEdge; 4] = [
    BaseEdge::mandatory("DRAFT", "PENDING_POST", "提交待过账", false),
    BaseEdge::mandatory("PENDING_POST", "POSTED", "过账", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("PENDING_POST", "CANCELLED", "待过账作废", true),
];

/// 应收单的 BASE_EDGES（与 [`RECEIVABLE_EDGES`] 逐条对应，顺序一致）。
///
/// 六条全 `mandatory = true`：收款推进、坏账核销、出库作废冲销都是账务动作，
/// **没有可选边**（`state_machine` 单测 `optional_edges_are_exactly_the_documented_four`
/// 钉死了「可选边只有 4 条」，这里多一条 optional 会直接红）。
pub const RECEIVABLE_BASE: [BaseEdge; 9] = [
    BaseEdge::mandatory("OPEN", "PARTIAL", "部分收款", false),
    BaseEdge::mandatory("OPEN", "SETTLED", "一次收清", false),
    BaseEdge::mandatory("PARTIAL", "SETTLED", "收清尾款", false),
    BaseEdge::mandatory("OPEN", "WRITTEN_OFF", "坏账核销", true),
    BaseEdge::mandatory("PARTIAL", "WRITTEN_OFF", "坏账核销", true),
    BaseEdge::mandatory("OPEN", "CANCELLED", "出库作废冲销应收", true),
    // 收款退款单的反向边（SO-14 遗留卡）：退款冲减把已收金额退回去，状态往回走。
    // 不要求 reason —— 依据是退款单本身（`biz_ref` 指向 `RECEIPT_REFUND` 单号），
    // 与收款侧的收款推进三条边同口径。
    BaseEdge::mandatory("SETTLED", "PARTIAL", "退款冲减（退回部分已收）", false),
    BaseEdge::mandatory("PARTIAL", "OPEN", "退款冲减（已收全额退回）", false),
    BaseEdge::mandatory("SETTLED", "OPEN", "退款冲减（已收全额退回）", false),
];

/// 收款退款单的 BASE_EDGES（与 [`RECEIPT_REFUND_EDGES`] 逐条对应，顺序一致）。
///
/// 三条全 `mandatory = true`：确认 / 过账 / 作废都是账务动作，没有可选边
/// （`optional_edges_are_exactly_the_documented_four` 钉死「可选边只有 4 条」）。
pub const RECEIPT_REFUND_BASE: [BaseEdge; 3] = [
    BaseEdge::mandatory("DRAFT", "CONFIRMED", "确认退款", false),
    BaseEdge::mandatory("CONFIRMED", "POSTED", "退款过账", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", true),
];

/// 收款单的 BASE_EDGES（与 [`RECEIPT_EDGES`] 逐条对应，顺序一致）。
///
/// 三条全 `mandatory = true`：确认 / 过账 / 作废都是账务动作，没有可选边
/// （`optional_edges_are_exactly_the_documented_four` 钉死「可选边只有 4 条」）。
pub const RECEIPT_BASE: [BaseEdge; 3] = [
    BaseEdge::mandatory("DRAFT", "CONFIRMED", "确认收款", false),
    BaseEdge::mandatory("CONFIRMED", "POSTED", "收款过账", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", true),
];

/// 销售发票的 BASE_EDGES（与 [`INVOICE_EDGES`] 逐条对应，顺序一致）。
///
/// 四条全 `mandatory`：开票 / 归档 / 两种作废都是账务动作，没有可选捷径
/// （`optional_edges_are_exactly_the_documented_four` 钉死「可选边只有 4 条」）。
pub const INVOICE_BASE: [BaseEdge; 4] = [
    BaseEdge::mandatory("DRAFT", "ISSUED", "开票（生成收入凭证）", false),
    BaseEdge::mandatory("ISSUED", "POSTED", "归档", false),
    BaseEdge::mandatory("DRAFT", "VOID", "草稿作废", true),
    BaseEdge::mandatory("ISSUED", "VOID", "开票后作废（红冲）", true),
];

/// 销售退货单的 BASE_EDGES（与 [`SALES_RETURN_EDGES`] 逐条对应，顺序一致）。
///
/// 七条全 `mandatory = true`：主链（提交 → 批准 → 待过账 → 过账）与三个作废口，
/// **没有可选边可关**（关掉「过账」等于让退货单永远停在待过账）。
pub const SALES_RETURN_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("APPROVED", "PENDING_POST", "进入待过账", false),
    BaseEdge::mandatory("PENDING_POST", "POSTED", "过账（回库 + 冲应收 + 红字凭证）", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "提交后作废", true),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "批准后作废", true),
];

/// 采购退货单的 BASE_EDGES（与 [`PURCHASE_RETURN_EDGES`] 逐条对应，顺序一致）。
///
/// 六条全 `mandatory = true`：主链（提交 → 批准 → 确认）与三个作废口，
/// **没有可选边可关**（关掉「确认」等于让退货单永远停在已批准）。
pub const PURCHASE_RETURN_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交审批", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("APPROVED", "CONFIRMED", "确认退货（出库 + 冲应付 + 凭证）", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "审批中作废", true),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "批准后作废", true),
];

/// 库存调拨单的 BASE_EDGES（与 [`TRANSFER_EDGES`] 逐条对应，顺序一致）。
///
/// 调拨六条边全是主链 / 撤销，**没有可选边**（没有捷径可关）。
pub const TRANSFER_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("DRAFT", "PENDING", "提交审批", false),
    BaseEdge::mandatory("PENDING", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("PENDING", "CANCELLED", "审批中取消", true),
    BaseEdge::mandatory("APPROVED", "IN_TRANSIT", "发货在途", false),
    BaseEdge::mandatory("IN_TRANSIT", "DONE", "收货完成", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", true),
];

/// 应付单的 BASE_EDGES（与 [`PAYABLE_EDGES`] 逐条对应，顺序一致）。
///
/// 付款与作废都是账务动作，**没有可选边**。
pub const PAYABLE_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("OPEN", "PARTIAL_PAID", "部分付款", false),
    BaseEdge::mandatory("OPEN", "SETTLED", "一次性付清", false),
    BaseEdge::mandatory("PARTIAL_PAID", "SETTLED", "付清尾款", false),
    BaseEdge::mandatory("OPEN", "CANCELLED", "作废", true),
    BaseEdge::mandatory("OPEN", "PENDING_CANCEL", "发起作废审批", true),
    BaseEdge::mandatory("PENDING_CANCEL", "CANCELLED", "审批通过作废", true),
    BaseEdge::mandatory("PENDING_CANCEL", "OPEN", "驳回回未付", false),
];

/// 凭证的 BASE_EDGES（与 [`VOUCHER_EDGES`] 逐条对应，顺序一致）。
///
/// 三条全 `mandatory`：审核 / 反审核 / 记账都是内控动作，**没有可选捷径**。
/// 尤其不能把 `AUDITED → POSTED` 配成可选再关掉 —— 关掉等于「凭证永远记不了账」，
/// 账本停在 AUDITED，总账 / 报表一条不进。
pub const VOUCHER_BASE: [BaseEdge; 3] = [
    BaseEdge::mandatory("DRAFT", "AUDITED", "审核", false),
    BaseEdge::mandatory("AUDITED", "DRAFT", "反审核", true),
    BaseEdge::mandatory("AUDITED", "POSTED", "记账", false),
];

/// 会计期间的 BASE_EDGES（与 [`PERIOD_EDGES`] 逐条对应，顺序一致）。
pub const PERIOD_BASE: [BaseEdge; 3] = [
    BaseEdge::mandatory("NOT_OPENED", "OPEN", "开账", false),
    BaseEdge::mandatory("OPEN", "CLOSED", "结账", false),
    BaseEdge::mandatory("CLOSED", "OPEN", "反结账", true),
];

/// 年结的 BASE_EDGES（与 [`YEAR_END_EDGES`] 逐条对应，顺序一致）。
pub const YEAR_END_BASE: [BaseEdge; 2] = [
    BaseEdge::mandatory("NOT_CLOSED", "CLOSED", "年结", false),
    BaseEdge::mandatory("CLOSED", "NOT_CLOSED", "反年结", true),
];

/// 合同的 BASE_EDGES（与 [`CONTRACT_EDGES`] 逐条对应，顺序一致）。
///
/// 三条新边全是 `mandatory`：提交审批（唯一入口）、审批落地（回调要能写 SIGNED）、
/// 驳回回位（审批驳回 / 撤回 / 终止后单据必须回到可编辑态）—— 被配置禁用就是
/// 「单据卡死在审批中」或「审批回调 500」，属于加严的反面。
pub const CONTRACT_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("DRAFT", "PENDING_APPROVAL", "提交审批", false),
    BaseEdge::mandatory("PENDING_APPROVAL", "SIGNED", "审批通过签订", false),
    BaseEdge::mandatory("PENDING_APPROVAL", "DRAFT", "驳回/撤回", false),
    BaseEdge::mandatory("SIGNED", "ACTIVE", "生效", false),
    BaseEdge::mandatory("SIGNED", "TERMINATED", "已签未生效终止", true),
    BaseEdge::mandatory("ACTIVE", "COMPLETED", "履约完成", false),
    BaseEdge::mandatory("ACTIVE", "TERMINATED", "中途终止", true),
];

/// 预算的 BASE_EDGES（与 [`BUDGET_EDGES`] 逐条对应，顺序一致）。
///
/// 全部 `mandatory`：提交审批（唯一入口）、驳回回位、审批落地（回调要能写 ACTIVE）、
/// 被取代（版本切换的核心）、年结关闭、两条作废边 —— 任何一条被配置禁用都会让
/// 「预算卡死在某个状态」，属于加严的反面。
pub const BUDGET_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交审批", false),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "驳回/撤回", true),
    BaseEdge::mandatory("SUBMITTED", "ACTIVE", "审批通过生效", false),
    BaseEdge::mandatory("ACTIVE", "SUPERSEDED", "被新版本取代", true),
    BaseEdge::mandatory("ACTIVE", "CLOSED", "年度结账关闭", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("SUBMITTED", "CANCELLED", "审批中作废", true),
];

/// 固定资产的 BASE_EDGES（与 [`FIXED_ASSET_EDGES`] 逐条对应，顺序一致）。
///
/// **五条全是 mandatory**：没有「捷径」可配 —— 停用 / 启用 / 处置 / 作废处置
/// 每一步都是内控动作，配置层不许把它们关掉（关掉等于资产永远停在某个状态）。
pub const FIXED_ASSET_BASE: [BaseEdge; 5] = [
    BaseEdge::mandatory("IN_USE", "IDLE", "停用（闲置，照提折旧）", true),
    BaseEdge::mandatory("IDLE", "IN_USE", "启用（重新投用）", true),
    BaseEdge::mandatory("IN_USE", "DISPOSED", "处置（审批通过后清账）", true),
    BaseEdge::mandatory("IDLE", "DISPOSED", "处置（闲置资产清账）", true),
    BaseEdge::mandatory("DISPOSED", "IN_USE", "作废处置（红冲后恢复在役）", true),
];

/// 折旧批次的 BASE_EDGES（与 [`DEPRECIATION_EDGES`] 逐条对应，顺序一致）。
///
/// 四条全 mandatory：一条计提 / 一次红冲 / 一次重跑都是财务动作，
/// 没有「可选捷径」这种东西。
pub const DEPRECIATION_BASE: [BaseEdge; 4] = [
    BaseEdge::mandatory("CALCULATED", "POSTED", "生成凭证并回写卡片", false),
    BaseEdge::mandatory("POSTED", "REVERSED", "红冲（红字凭证 + 回滚卡片）", true),
    BaseEdge::mandatory("CALCULATED", "SKIPPED", "跳过（本期无应提折旧）", false),
    BaseEdge::mandatory("REVERSED", "CALCULATED", "红冲后重跑（复用同一批次行）", true),
];

/// 出库单的 BASE_EDGES（与 [`STOCK_OUT_EDGES`] 逐条对应，顺序一致）。
///
/// 可选边只有 `DRAFT → POSTED`（跳过过账审批直接过账）。
pub const STOCK_OUT_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("DRAFT", "PENDING_POST", "提交待过账", false),
    BaseEdge::mandatory("PENDING_POST", "POSTED", "过账", false),
    BaseEdge::mandatory("PENDING_POST", "DRAFT", "撤回", false),
    BaseEdge::optional("DRAFT", "POSTED", "直接过账", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("POSTED", "CANCELLED", "已过账作废", true),
];

/// 盘点单的 BASE_EDGES（与 [`STOCK_TAKE_EDGES`] 逐条对应，顺序一致）。
///
/// **没有可选边**：`DRAFT → POSTED` 在盘点上是禁止的（必须先审批 + 独立过账）。
pub const STOCK_TAKE_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("DRAFT", "PENDING_POST", "提交审批", false),
    BaseEdge::mandatory("PENDING_POST", "POSTED", "过账", false),
    BaseEdge::mandatory("PENDING_POST", "DRAFT", "撤回/驳回", false),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "草稿作废", false),
    BaseEdge::mandatory("PENDING_POST", "CANCELLED", "待过账作废", false),
    BaseEdge::mandatory("POSTED", "CANCELLED", "已过账作废", true),
];

// ==================== 主链路「必需边」（关掉它 = 功能断，不是加严） ====================
//
// 【E2E-FIX-1】2026-11-09 E2E-02 演练实际踩到：`biz_flow_state_edge` 里
// `STOCK_OUT DRAFT→POSTED` 被置成 `enabled = false`（属主不明），发货过账当场 409
// 「流转已被状态机配置禁用」——**整条销售链断在发货**，而界面上没有任何入口能看出
// 「这条边被关了」。标记 + 二次确认 + 巡检，都是为了防止同一个坑再踩一次。
//
// 这张表回答一个问题：**关掉这条边，是不是有业务功能直接做不了？**
//   · `impact` 是给用户看的业务后果（流转被拦时的错误文案 / 前端红字 / 巡检报警共用一句话，
//     句式固定为「让{impact}」与「{impact}：…」，所以写成「销售出库无法过账」这种从句）；
//   · 标进来的边，`upsert_edge_config` 要求 `confirm_critical = true` 才允许禁用；
//   · `scripts/verify/flow_critical_edges.sh` 只读巡检：任何一条 critical 边
//     `enabled = false` 就报警（**不自动改回**，避免把人工加严的合法动作覆盖掉）。
//
// # 与 `mandatory` 的分工（两张表都在代码里，谁是真相一目了然）
//
//   `mandatory = true`  → 配置层**根本禁不掉**（400；绕过 API 直接写库则忽略 + WARN）。
//                          主链 / 审批落地 / 撤销回位 / 红冲都在这一档。
//   `critical  = true`  → 可以禁，但必须**显式二次确认**，且巡检长期盯着。
//                          目前只有 optional 边是「真的能被关掉」的主链捷径
//                          （`STOCK_OUT DRAFT→POSTED` 发货免审直过、
//                           `STOCK_IN DRAFT→POSTED` 收货免审直过）；其余标进来的边是
//                          「将来若放开成 optional 也不许悄悄关」的保护。
//
// # 巡检表 `biz_flow_critical_edge`（V0354）与这张表的关系
//
//   巡检脚本是 bash + psql，读不到 Rust 常量，所以 V0354 把同一份清单种进
//   `biz_flow_critical_edge`（entity / from_status / to_status / impact）。
//   两边不许漂移：真库单测 `critical_edges_match_db_table` 逐行比对，漏一条 / 多一条直接红。
//   **改这张表必须同步加一条新迁移**（不能改已应用的 V0354）。

/// 一条「主链路必需边」：禁用它会让某个业务功能直接不可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalEdge {
    /// 实体类型。
    pub entity: Entity,
    /// 变更前状态。
    pub from: &'static str,
    /// 变更后状态。
    pub to: &'static str,
    /// 业务后果（给用户看的句子，例：`销售出库无法过账`）。
    pub impact: &'static str,
}

impl CriticalEdge {
    /// 构造（全部 const，编译期就能查重）。
    const fn new(entity: Entity, from: &'static str, to: &'static str, impact: &'static str) -> Self {
        Self {
            entity,
            from,
            to,
            impact,
        }
    }
}

/// 全部主链路必需边（顺序 = 业务链顺序：销售出库 → 采购收货 → 采购订单 → 销售订单 →
/// 发货 → 调拨 → 盘点 → 凭证）。
///
/// 卡面写的 `STOCK_TRANSFER DRAFT → DONE` 在 BASE_EDGES 里**不存在**（调拨必须走
/// `DRAFT→PENDING→APPROVED→IN_TRANSIT→DONE`），这里按真实边表补成四段。
pub const CRITICAL_EDGES: &[CriticalEdge] = &[
    // 销售出库（E2E-02 断链的那一条就是它）
    CriticalEdge::new(Entity::StockOut, "DRAFT", "PENDING_POST", "销售出库无法提交过账审批"),
    CriticalEdge::new(Entity::StockOut, "PENDING_POST", "POSTED", "销售出库无法过账"),
    CriticalEdge::new(Entity::StockOut, "DRAFT", "POSTED", "销售出库无法直接过账"),
    // 采购收货
    CriticalEdge::new(Entity::StockIn, "DRAFT", "PENDING_POST", "采购收货无法提交过账审批"),
    CriticalEdge::new(Entity::StockIn, "PENDING_POST", "POSTED", "采购收货无法过账"),
    CriticalEdge::new(Entity::StockIn, "DRAFT", "POSTED", "采购收货无法直接过账"),
    // 采购订单
    CriticalEdge::new(Entity::PurchaseOrder, "DRAFT", "SUBMITTED", "采购订单无法提交审批"),
    CriticalEdge::new(
        Entity::PurchaseOrder,
        "SUBMITTED",
        "APPROVED",
        "采购订单无法审批通过（后续收货全部卡住）",
    ),
    CriticalEdge::new(
        Entity::PurchaseOrder,
        "APPROVED",
        "PARTIAL_RECEIVED",
        "采购订单无法继续收货",
    ),
    CriticalEdge::new(Entity::PurchaseOrder, "APPROVED", "RECEIVED", "采购订单无法收货完成"),
    // 销售订单
    CriticalEdge::new(Entity::SaleOrder, "DRAFT", "SUBMITTED", "销售订单无法提交审批"),
    CriticalEdge::new(
        Entity::SaleOrder,
        "SUBMITTED",
        "APPROVED",
        "销售订单无法审批通过（后续发货全部卡住）",
    ),
    CriticalEdge::new(Entity::SaleOrder, "APPROVED", "PARTIAL_OUT", "销售订单无法部分出库"),
    CriticalEdge::new(Entity::SaleOrder, "APPROVED", "OUTBOUND", "销售订单无法出库完成"),
    CriticalEdge::new(Entity::SaleOrder, "PARTIAL_OUT", "OUTBOUND", "销售订单无法出库完成"),
    // 发货单
    CriticalEdge::new(Entity::Shipment, "DRAFT", "PENDING_POST", "发货单无法提交待过账"),
    CriticalEdge::new(Entity::Shipment, "PENDING_POST", "POSTED", "发货单无法过账"),
    // 库存调拨
    CriticalEdge::new(Entity::Transfer, "DRAFT", "PENDING", "库存调拨无法提交审批"),
    CriticalEdge::new(Entity::Transfer, "PENDING", "APPROVED", "库存调拨无法审批通过"),
    CriticalEdge::new(Entity::Transfer, "APPROVED", "IN_TRANSIT", "库存调拨无法发货"),
    CriticalEdge::new(Entity::Transfer, "IN_TRANSIT", "DONE", "库存调拨无法收货完成"),
    // 盘点
    CriticalEdge::new(Entity::StockTake, "DRAFT", "PENDING_POST", "盘点单无法提交审批"),
    CriticalEdge::new(Entity::StockTake, "PENDING_POST", "POSTED", "盘点单无法过账"),
    // 凭证（记账链）
    CriticalEdge::new(Entity::Voucher, "DRAFT", "AUDITED", "凭证无法审核"),
    CriticalEdge::new(
        Entity::Voucher,
        "AUDITED",
        "POSTED",
        "凭证无法记账（总账与报表一条不进）",
    ),
];

/// 这条边是不是主链路必需边（`None` = 不是）。
pub fn critical_edge(entity: Entity, from: &str, to: &str) -> Option<&'static CriticalEdge> {
    CRITICAL_EDGES
        .iter()
        .find(|item| item.entity == entity && item.from == from && item.to == to)
}

// ==================== 成本核算 · 成本对象（ARCH-9 §4.1） ====================

/// 实体类型：成本对象（生产批）`biz_cost_object`。
///
/// 做法同 `ENTITY_FIXED_ASSET`：落库值取 [`DocKind::CostObject`]（`"COST_OBJECT"`），
/// 不是手写字面量 —— `doc_kind_tests::entity_registry_is_covered_by_doc_kind`
/// 要求注册表里每个实体都在 `DocKind` 域内。
pub const ENTITY_COST_OBJECT: &str = DocKind::CostObject.as_str();

/// 成本对象合法边（ARCH-9 §4.1，只有 2 条）。
///
/// ```text
/// OPEN ──关批──▶ CLOSED        成本已结转，历史数据全保留
/// OPEN ──作废──▶ CANCELLED     建错了（前置：无归集行、无入库、无结算明细）
/// ```
///
/// **没有 `CLOSED → OPEN`**：设计 §4.1 写死「成本已结转，重开会破坏已结算期间的
/// 归集基线；要复工就新建批」。这条缺失是**故意的**，别在调用点补。
///
/// **没有 `CANCELLED → *`**：作废是终态（只读）。
pub const COST_OBJECT_EDGES: [(&str, &str); 2] = [("OPEN", "CLOSED"), ("OPEN", "CANCELLED")];

/// 需要 reason 的成本对象边：**只有作废**。
///
/// 关批的备注（`remark`）是可选的业务说明，不是「影响已入账数据」的解释；
/// 作废则是把「建错的批」从核算范围里拿掉，必须留下为什么作废（审计口径）。
pub const COST_OBJECT_REASON_EDGES: [(&str, &str); 1] = [("OPEN", "CANCELLED")];

/// 成本对象「不可改」状态：关批 / 作废之后只读
/// （`ensure_writable` 用它拦「已关批还被改基本信息」）。
pub const COST_OBJECT_LOCKED: [&str; 2] = ["CLOSED", "CANCELLED"];

/// 成本对象的 BASE_EDGES（与 [`COST_OBJECT_EDGES`] 逐条对应，顺序一致）。
///
/// 两条全 mandatory：关批与作废都是**改动核算基线**的动作，
/// 配置层不许把它们关掉（关掉等于对象永远停在 OPEN、永远结算不下去）。
pub const COST_OBJECT_BASE: [BaseEdge; 2] = [
    BaseEdge::mandatory("OPEN", "CLOSED", "关批（成本已结转）", false),
    BaseEdge::mandatory("OPEN", "CANCELLED", "作废（建错了）", true),
];

// ==================== 成本核算 · 核算批次（ARCH-9 §4.1） ====================

/// 实体类型：成本核算批次（期间结算单）`biz_cost_period`。
///
/// 落库值取 [`DocKind::CostPeriod`]（`"COST_PERIOD"`）—— 与 `ENTITY_COST_OBJECT`
/// 同款：`doc_kind_tests::entity_registry_is_covered_by_doc_kind` 要求注册表里
/// 每个实体都在 `DocKind` 域内，手写字面量会漂。
///
/// **卡面写的是 `DRAFT → PENDING → APPROVED`，落库值是 `SUBMITTED`**：设计 §3.1 ⑨ 的
/// `ck_biz_cost_period_status` 与 V0373 已按 `SUBMITTED` 落地（已应用迁移不可改），
/// 且全仓其余审批类单据（采购订单 / 销售订单 / 退货）都是 `SUBMITTED`。
/// 卡面的 `PENDING` 是同一个状态的通俗叫法，**不另立别名**（别名 = 兼容层永远在）。
pub const ENTITY_COST_PERIOD: &str = DocKind::CostPeriod.as_str();

/// 成本核算批次合法边（ARCH-9 §4.1，7 条）。
///
/// ```text
/// DRAFT ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED ──结算──▶ SETTLED ──期间结账──▶ CLOSED
///   │                  │                                        │
///   │                  └──驳回/撤回──▶ DRAFT                    └──红冲──▶ REVERSED
///   └──作废──▶ CANCELLED
/// ```
///
/// **没有 `DRAFT → APPROVED`**：设计 §4.1 写死「`DRAFT → APPROVED` 必须过审批流」，
/// 免审直过会让 `ck_biz_cost_period_appr`（`SUBMITTED` 起必须挂审批实例）失去意义。
/// **没有 `CLOSED → *`**：期间已结账，只读。
/// **没有 `REVERSED → *`**：红冲是终态；同期重建**新批次**（唯一键已排除 `REVERSED`）。
pub const COST_PERIOD_EDGES: [(&str, &str); 7] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "APPROVED"),
    ("SUBMITTED", "DRAFT"),
    ("APPROVED", "SETTLED"),
    ("SETTLED", "CLOSED"),
    ("SETTLED", "REVERSED"),
    ("DRAFT", "CANCELLED"),
];

/// 需要 reason 的成本核算批次边：**红冲 + 作废**。
///
/// `SUBMITTED → DRAFT` 故意**不在**这里：同一条边承载两个动作 ——
/// 「撤回」（提交人自己拿回，无需解释）与「驳回」（审批人给出意见）。
/// 若把它标成必填原因，「撤回」会被 400 挡住（正常业务路径直接断，LNK-P08 同款）。
/// 驳回意见由 B3-3 的接线写进 `remark`，不靠状态机的必填校验。
pub const COST_PERIOD_REASON_EDGES: [(&str, &str); 2] = [("SETTLED", "REVERSED"), ("DRAFT", "CANCELLED")];

/// 成本核算批次「不可改」状态：**只有 `DRAFT` 可写**。
///
/// 设计 §4.1 的口径：`DRAFT` 可反复重算；`SUBMITTED` 起数据冻结
/// （要改就撤回或作废重建）。`ensure_writable` 用它拦「已提交还被改」。
pub const COST_PERIOD_LOCKED: [&str; 6] = ["SUBMITTED", "APPROVED", "SETTLED", "CLOSED", "REVERSED", "CANCELLED"];

/// 成本核算批次的 BASE_EDGES（与 [`COST_PERIOD_EDGES`] 逐条对应，顺序一致）。
///
/// 七条全 mandatory：结算链的每一段（提交 → 审批 → 结算 → 关期 / 红冲）都是主链，
/// 配置层关掉任何一条都会让批次**永久停在某个中间态**（且没有兜底待办）。
/// 红冲与作废要原因（影响已入账数据 / 放弃整期核算）。
pub const COST_PERIOD_BASE: [BaseEdge; 7] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交审批", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "驳回 / 撤回", false),
    BaseEdge::mandatory("APPROVED", "SETTLED", "结算（入库 + 凭证）", false),
    BaseEdge::mandatory("SETTLED", "CLOSED", "期间结账", false),
    BaseEdge::mandatory("SETTLED", "REVERSED", "红冲结算", true),
    BaseEdge::mandatory("DRAFT", "CANCELLED", "作废", true),
];

// ==================== 人力资源（ARCH-9-人力资源模块设计 §4.2） ====================
//
// 五个实体（员工 / 劳动合同 / 请假单 / 考勤期间 / 薪资单）的落库值一律取 [`DocKind`]
// （HR-06 已扩这 5 个值），与 `ENTITY_COST_OBJECT` 同款：`doc_kind_tests::
// entity_registry_is_covered_by_doc_kind` 要求注册表里每个实体都在 `DocKind` 域内，
// 手写字面量会漂。`biz_status_log.entity_type` 无 CHECK 白名单（V0159 / V0219:44），
// 常量登记后 `transition()` 即可用。
//
// # 状态域的真相在 DDL，不在流转图
//
// 下面每张边表 / reason 表 / locked 表里出现的状态串**必须落在 V0383 的
// `ck_biz_*_status` 白名单内**（迁移已应用、不可改）。单测
// `hr_status_domains_match_ddl` 直接读 `migrations/business/V0383__hr_tables.up.sql`
// 逐条比对白名单，`hr_edge_statuses_stay_in_ddl_domain` 再保证边表不越域 ——
// 越域的一次写库就是 23514，而 `transition()` 先写 `biz_status_log`（它只写日志），
// 于是留下「日志说有这一步、单据上没有这个状态」的脏轨迹。
//
// 这条护栏不是洁癖：设计 §4.2.4 的薪资单流转图**画了 3 条 `→ CANCELLED`**，
// 而 §3.1 ⑧ 的 `ck_biz_sal_sheet_status` 只有 6 个值（**没有 CANCELLED**），
// HR-09 的 `ROUTE_CONTRACT` 薪资组也没有作废接口。照图落边 = 一按作废就 23514。

/// 实体类型：员工档案（`biz_employee`，设计 §3.1 ①）。
///
/// 落库值 `EMPLOYEE`。**员工不是账号**（设计 D-1：`biz_employee.user_id` 单向下挂
/// `sys_user`，账号表一列都不加），所以人事状态与 `sys_user.status`（0/1 启用停用）
/// 是**两套**：离职 = 人事 `RESIGNED` + 账号停用，两者都写、都不删。
pub const ENTITY_EMPLOYEE: &str = DocKind::Employee.as_str();

/// 员工合法边（设计 §4.2.1 的流转图，6 条）。
///
/// ```text
/// PROBATION ──转正──▶ ACTIVE ──停薪留职──▶ SUSPENDED ──复职──▶ ACTIVE
///      │                │                     │
///      └────────────────┴─────────────────────┴──▶ RESIGNED（终态）
/// ```
///
/// 三条入 `RESIGNED` 的边**只由离职审批回调写**（`hr_employee_resign` 通过后的
/// 第一步，设计 §4.1.3 ③）；`PROBATION → ACTIVE` 的守卫是「有生效中的劳动合同」
/// （设计 §4.2.1 表格），那是**调用点的字段/关联校验**，不是状态机的必填原因 ——
/// 所以它不在 [`EMPLOYEE_REASON_EDGES`] 里（放进去会让转正按钮因缺 reason 被 400 挡住，
/// LNK-P08 同款）。
///
/// **没有** `RESIGNED → *`：离职是终态（再入职 = 新员工档，历史单据靠
/// `employee_id` + 姓名/工号快照回溯，设计 §4.1.1）。
pub const EMPLOYEE_EDGES: [(&str, &str); 6] = [
    ("PROBATION", "ACTIVE"),
    ("ACTIVE", "SUSPENDED"),
    ("SUSPENDED", "ACTIVE"),
    ("PROBATION", "RESIGNED"),
    ("ACTIVE", "RESIGNED"),
    ("SUSPENDED", "RESIGNED"),
];

/// 需要 reason 的员工边（设计 §4.2.1 点名「原因 ≥ 5 字进 `biz_status_log.reason`」的两类动作）：
/// 停薪留职（1 条）+ 离职（3 条，三个在职状态都能离职）。
///
/// 设计写「3 条：转正/停薪/离职」是**按动作数**说的；落到边是 4 条 —— 离职有
/// `PROBATION / ACTIVE / SUSPENDED` 三个入口，而转正的守卫是「生效合同」不是原因
/// （见 [`EMPLOYEE_EDGES`]）。离职原因同时进 `biz_employee.leave_type` 与
/// `biz_status_log.reason`，审计要能回答「为什么走的」。
pub const EMPLOYEE_REASON_EDGES: [(&str, &str); 4] = [
    ("ACTIVE", "SUSPENDED"),
    ("PROBATION", "RESIGNED"),
    ("ACTIVE", "RESIGNED"),
    ("SUSPENDED", "RESIGNED"),
];

/// 员工「不可改」状态：已离职（设计 §4.2.1 逐字）。
///
/// 离职档只读的理由：它是在职期间的**主数据基线**，改入职日期 / 部门会让历史薪资单
/// 的快照对不上（薪资明细存了 `dept_name` 快照，主数据改名不影响工资表 —— 设计 §3.1 ⑨）。
pub const EMPLOYEE_LOCKED: [&str; 1] = ["RESIGNED"];

/// 员工的 BASE_EDGES（与 [`EMPLOYEE_EDGES`] 逐条对应，顺序一致，全部 mandatory）。
///
/// 六条都是人事主链：转正 / 停薪 / 复职 / 离职。没有「捷径」可关 ——
/// 关掉 `PROBATION → ACTIVE` 等于试用期员工永远转不正，关掉 `* → RESIGNED` 等于
/// 离职审批通过了却写不进状态（审批回调 500，单据卡在审批中）。
pub const EMPLOYEE_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("PROBATION", "ACTIVE", "转正（需生效劳动合同）", false),
    BaseEdge::mandatory("ACTIVE", "SUSPENDED", "停薪留职", true),
    BaseEdge::mandatory("SUSPENDED", "ACTIVE", "复职", false),
    BaseEdge::mandatory("PROBATION", "RESIGNED", "试用期离职（审批通过）", true),
    BaseEdge::mandatory("ACTIVE", "RESIGNED", "离职（审批通过）", true),
    BaseEdge::mandatory("SUSPENDED", "RESIGNED", "停薪期间离职（审批通过）", true),
];

/// 实体类型：劳动合同（`biz_employee_contract`，设计 §3.1 ②）。
///
/// 落库值 `EMPLOYEE_CONTRACT`。一份合同一行、续签链式（`renewed_from_id`），
/// 所以「员工的合同」永远是多行历史，最新生效的那行 `status = 'ACTIVE'`。
pub const ENTITY_EMPLOYEE_CONTRACT: &str = DocKind::EmployeeContract.as_str();

/// 劳动合同合法边（4 条；设计只给了状态域与「续签链」，边表按状态语义落）。
///
/// ```text
/// ACTIVE ──到期──▶ EXPIRED ──补签续签──▶ RENEWED
///    │                 
///    ├──解除──▶ TERMINATED
///    └──续签──▶ RENEWED
/// ```
///
/// - `ACTIVE → EXPIRED`：到期扫描（`idx_biz_emp_contract_due` 正是按
///   `status='ACTIVE' AND end_date` 建的索引，设计 §3.1 ②），由定时任务/查询写。
/// - `ACTIVE → RENEWED`：**到期前**续签是常态（新合同从明天起，旧合同当场封闭）——
///   只放开 `EXPIRED → RENEWED` 会让最常见的续签（提前办）直接 409。
/// - `EXPIRED → RENEWED`：到期后才补签。
/// - `ACTIVE → TERMINATED`：解除（`terminated_date` + `terminate_reason` 由库层
///   `ck_biz_emp_contract_term` 双向约束）。
///
/// **没有 `EXPIRED → TERMINATED`**：自然到期的合同不需要「解除」，两者混用会让
/// 「合同解除率」统计失真。**没有 `RENEWED / TERMINATED → *`**：两者都是封闭终态，
/// 续签链只往前长。
pub const EMPLOYEE_CONTRACT_EDGES: [(&str, &str); 4] = [
    ("ACTIVE", "EXPIRED"),
    ("ACTIVE", "TERMINATED"),
    ("ACTIVE", "RENEWED"),
    ("EXPIRED", "RENEWED"),
];

/// 需要 reason 的合同边：只有解除（`ACTIVE → TERMINATED`）。
///
/// `terminate_reason` 是解除的法律依据（V0383 的列注释：255 字），审计要能回答
/// 「哪份、为什么解除」。到期是自然事件、续签的依据是新合同本身（`renewed_from_id` 链），
/// 都不需要额外解释。
pub const EMPLOYEE_CONTRACT_REASON_EDGES: [(&str, &str); 1] = [("ACTIVE", "TERMINATED")];

/// 合同「不可改」状态：已解除 / 已续签。
///
/// `EXPIRED` 故意**不锁**：到期合同最常见的操作就是「补正录入错的 end_date 再续签」
/// （`ACTIVE` 记录一旦到期，错的日期就在 `EXPIRED` 行上），锁死只会逼 HR 删单重建。
pub const EMPLOYEE_CONTRACT_LOCKED: [&str; 2] = ["TERMINATED", "RENEWED"];

/// 劳动合同的 BASE_EDGES（与 [`EMPLOYEE_CONTRACT_EDGES`] 逐条对应，顺序一致）。
pub const EMPLOYEE_CONTRACT_BASE: [BaseEdge; 4] = [
    BaseEdge::mandatory("ACTIVE", "EXPIRED", "到期（到期扫描）", false),
    BaseEdge::mandatory("ACTIVE", "TERMINATED", "解除劳动合同", true),
    BaseEdge::mandatory("ACTIVE", "RENEWED", "续签（旧合同封闭，新合同接链）", false),
    BaseEdge::mandatory("EXPIRED", "RENEWED", "到期后补签续签", false),
];

/// 实体类型：请假单（`biz_leave_request`，设计 §3.1 ⑥）。
///
/// 落库值 `LEAVE_REQUEST`。审批 `biz_type = HR_LEAVE`（HR-06），回调写
/// `SUBMITTED → APPROVED`；「作废」是 HR 侧动作（销假/录错）。
pub const ENTITY_LEAVE_REQUEST: &str = DocKind::LeaveRequest.as_str();

/// 请假单合法边（设计 §4.2.2 的流转图，4 条）。
///
/// ```text
/// DRAFT ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED ──销假/作废──▶ CANCELLED
///   │                  │
///   │                  └──驳回/撤回──▶ DRAFT
///   └──删除（软删）──▶ deleted_at（不是状态）
/// ```
///
/// **草稿作废走软删（`deleted_at`），不是 `DRAFT → CANCELLED`**：设计图把
/// 「DRAFT 删除」单独画成 `deleted_at` 分支 —— 草稿从未提交过，`CANCELLED`
/// 留给「已批准的假要销掉」（`cancel_reason` 由库层 `ck_biz_leave_cancel` 强制）。
///
/// **没有 `SUBMITTED → CANCELLED`**：审批中的假要撤就先驳回/撤回到 `DRAFT`
/// （`SUBMITTED → DRAFT` 已登记），撤回的审批实例由 `cancel_approval_of` 终止。
/// 少一条边就少一处「审批还在跑、单据已作废」的窗口（`uk_biz_flow_instance_biz_active`）。
pub const LEAVE_EDGES: [(&str, &str); 4] = [
    ("DRAFT", "SUBMITTED"),
    ("SUBMITTED", "DRAFT"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "CANCELLED"),
];

/// 需要 reason 的请假单边：只有作废已批准的假（设计 §4.2.2「销假/作废(给原因)」）。
///
/// `SUBMITTED → DRAFT` 不强制原因：同一条边承载「撤回」（申请人自己拿回）与
/// 「驳回」（审批人给意见，意见由审批实例留痕），标成必填会把撤回挡成 400。
pub const LEAVE_REASON_EDGES: [(&str, &str); 1] = [("APPROVED", "CANCELLED")];

/// 请假单「不可改」状态：已批准 / 已作废（设计 §4.2.2 逐字 `LEAVE_LOCKED`）。
///
/// 批准后改天数 = 改一个已经影响考勤扣款与薪资试算的基线（`ATT_LEAVE_UNPAID_DAYS`），
/// 要改只能作废重开。
pub const LEAVE_LOCKED: [&str; 2] = ["APPROVED", "CANCELLED"];

/// 请假单的 BASE_EDGES（与 [`LEAVE_EDGES`] 逐条对应，顺序一致，全部 mandatory）。
pub const LEAVE_BASE: [BaseEdge; 4] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交审批", false),
    BaseEdge::mandatory("SUBMITTED", "DRAFT", "驳回 / 撤回", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("APPROVED", "CANCELLED", "销假 / 作废", true),
];

/// 实体类型：考勤期间（`biz_attendance_period`，设计 §3.1 ④）。
///
/// 落库值 `ATTENDANCE_PERIOD`。这是**锁定型作业批次**（不是单据，所以
/// `DocKind::is_document()` 为 false、不进附件轴）：一人一月一行的
/// `biz_attendance_record` 都挂它，锁定后就是薪资试算的数据基线。
pub const ENTITY_ATTENDANCE_PERIOD: &str = DocKind::AttendancePeriod.as_str();

/// 考勤期间合法边（设计 §4.2.3，3 条）。
///
/// ```text
/// DRAFT ──提交──▶ SUBMITTED ──锁定──▶ LOCKED ──解锁（给原因）──▶ DRAFT
/// ```
///
/// 锁定是**人事自己锁**、不校验审批（设计 §4.2.3），但要 `business:hr:attendance:lock`
/// 权限码并且写 `biz_status_log`。解锁必须给原因：解开的是一份已经被工资试算用过的
/// 基线（设计 §4.2.3「薪资单引用锁定的考勤期间才允许过账」）。
///
/// **没有 `SUBMITTED → DRAFT`**：设计图只有「提交 → 锁定」一条正向与「锁定 → 解锁」
/// 一条回位；提交态可以直接重新导入（导入落在记录上，不依赖期间状态）。
pub const ATTENDANCE_PERIOD_EDGES: [(&str, &str); 3] =
    [("DRAFT", "SUBMITTED"), ("SUBMITTED", "LOCKED"), ("LOCKED", "DRAFT")];

/// 需要 reason 的考勤期间边：只有解锁（设计 §4.2.3「解锁(给原因…)」）。
pub const ATTENDANCE_PERIOD_REASON_EDGES: [(&str, &str); 1] = [("LOCKED", "DRAFT")];

/// 考勤期间「不可改」状态：已锁定（设计 §4.2.3「`LOCKED` 之后记录不许增删改」）。
///
/// `SUBMITTED` 故意不锁：提交后还要能重跑导入（客户第一轮导入几乎总有错行）。
pub const ATTENDANCE_PERIOD_LOCKED: [&str; 1] = ["LOCKED"];

/// 考勤期间的 BASE_EDGES（与 [`ATTENDANCE_PERIOD_EDGES`] 逐条对应，顺序一致）。
pub const ATTENDANCE_PERIOD_BASE: [BaseEdge; 3] = [
    BaseEdge::mandatory("DRAFT", "SUBMITTED", "提交", false),
    BaseEdge::mandatory("SUBMITTED", "LOCKED", "锁定（记录冻结）", false),
    BaseEdge::mandatory("LOCKED", "DRAFT", "解锁", true),
];

/// 实体类型：薪资单（`biz_salary_sheet`，设计 §3.1 ⑧）。
///
/// 落库值 `SALARY_SHEET`。**状态域 6 值**：`DRAFT / CALCULATED / SUBMITTED /
/// APPROVED / POSTED / REVERSED` —— 与 V0383 的 `ck_biz_sal_sheet_status` 逐字一致，
/// **没有 `CANCELLED`**（见本节顶部与设计 §4.2.4 的冲突说明）。
pub const ENTITY_SALARY_SHEET: &str = DocKind::SalarySheet.as_str();

/// 薪资单合法边（6 条 = 5 条主链 + 1 条驳回回位）。
///
/// ```text
/// DRAFT ──试算──▶ CALCULATED ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED ──过账──▶ POSTED ──红冲──▶ REVERSED
///                                        │
///                                        └──驳回 / 撤回──▶ CALCULATED
/// ```
///
/// # 为什么不是设计 §4.2.4 写的「7 条」
///
/// 设计 §4.2.4 的流转图画了 `DRAFT / CALCULATED / SUBMITTED → CANCELLED` 三条作废边
/// （图上括号标「仅 DRAFT/CALCULATED/SUBMITTED 可作废」）。但 §3.1 ⑧ 的
/// `ck_biz_sal_sheet_status` 只有 6 个值、**没有 `CANCELLED`**，V0383 已按 6 值落库
/// （迁移不可改），HR-09 的 `ROUTE_CONTRACT` 薪资组也没有作废接口。三条作废边
/// 一条都落不了：写进去就是 23514。
///
/// 于是薪资单的退出只有一条：**驳回/撤回退回 `CALCULATED` 重算**。这条回位边不是
/// 「顺手加的」—— `SUBMITTED` 在 [`SALARY_SHEET_LOCKED`] 里（明细冻结），既没有作废
/// 接口也没有退回边的话，一张被财务经理驳回来的薪资单会**永久卡在 SUBMITTED**：
/// 过不了账（要 APPROVED）、改不了数（锁死）、也没有待办会来推进它 —— 正是
/// `AGENTS.md` 约定 11 说的死单。薪资期间撞唯一键（`uk_biz_sal_sheet_period`）也不允许
/// 「删了重开」当常规路径。
///
/// 同时**没有 `REVERSED → CALCULATED`**：设计 §4.2.4 写死「红冲是终态，不回到
/// CALCULATED」（红冲后再改数会留下「红字凭证 + 新数」两套口径）。要重发就新开一张。
pub const SALARY_SHEET_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "CALCULATED"),
    ("CALCULATED", "SUBMITTED"),
    ("SUBMITTED", "CALCULATED"),
    ("SUBMITTED", "APPROVED"),
    ("APPROVED", "POSTED"),
    ("POSTED", "REVERSED"),
];

/// 需要 reason 的薪资单边：只有红冲（设计 §4.2.4 / `reverse_reason` 列）。
///
/// `SUBMITTED → CALCULATED` 不强制原因：同一条边承载「撤回」（HR 自己拿回重算）与
/// 「驳回」（审批意见由审批实例留痕），标成必填会把撤回挡成 400（LNK-P08 同款）。
pub const SALARY_SHEET_REASON_EDGES: [(&str, &str); 1] = [("POSTED", "REVERSED")];

/// 薪资单「不可改」状态（设计 §4.2.4 逐字：`SUBMITTED` / `APPROVED` / `POSTED` / `REVERSED`）。
///
/// `DRAFT` 与 `CALCULATED` 可改：试算可在 `DRAFT` 反复跑，`CALCULATED` 之后还有
/// 明细手工覆盖（HR-37），两者都还没进审批。`SUBMITTED` 起冻结 —— 审批人批的是
/// 「这一版金额」，批完立刻按它出两张凭证，中途改一行就是「审 A 记 B」。
pub const SALARY_SHEET_LOCKED: [&str; 4] = ["SUBMITTED", "APPROVED", "POSTED", "REVERSED"];

/// 薪资单的 BASE_EDGES（与 [`SALARY_SHEET_EDGES`] 逐条对应，顺序一致，全部 mandatory）。
///
/// 过账（`APPROVED → POSTED`）尤其不能做成可选边：关掉它等于「审批过了但永远入不了账」，
/// 而 `AGENTS.md` 约定 11 的红线就是「审批过了没入账」这种中间态。
pub const SALARY_SHEET_BASE: [BaseEdge; 6] = [
    BaseEdge::mandatory("DRAFT", "CALCULATED", "试算（逐人逐项落库）", false),
    BaseEdge::mandatory("CALCULATED", "SUBMITTED", "提交审批", false),
    BaseEdge::mandatory("SUBMITTED", "CALCULATED", "驳回 / 撤回（退回重算）", false),
    BaseEdge::mandatory("SUBMITTED", "APPROVED", "审批通过", false),
    BaseEdge::mandatory("APPROVED", "POSTED", "过账（计提 + 发放两张凭证）", false),
    BaseEdge::mandatory("POSTED", "REVERSED", "红冲（两张红字凭证）", true),
];

/// 单据类型注册表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    /// 收货入库单 `biz_stock_in`。
    StockIn,
    /// 采购订单 `biz_purchase_order`。
    PurchaseOrder,
    /// 库存调拨单 `biz_stock_transfer`。
    Transfer,
    /// 应付单 `biz_payable`。
    Payable,
    /// 凭证 `biz_voucher`。
    Voucher,
    /// 会计期间 `biz_accounting_period`（ERP-P1）。
    Period,
    /// 会计年度（年结）（ERP-R4）。
    YearEnd,
    /// 合同 `biz_contract`（A1 合同表基座，ARCH-2）。
    Contract,
    /// 出库单 `biz_stock_out`（ARCH-3 DC-13）。
    StockOut,
    /// 盘点单 `biz_stock_take`（ARCH-3 DC-14）。
    StockTake,
    /// 预算 `biz_budget`（ARCH-6 第三章 · FIN-1）。
    Budget,
    /// 固定资产 `biz_fixed_asset`（ARCH-6 第五章 · FIN-2）。
    FixedAsset,
    /// 折旧批次 `biz_depreciation_batch`（ARCH-6 §5.5.5 · FIN-2）。
    Depreciation,
    /// 费用报销单 `biz_expense_report`（ARCH-6 第四章 · FIN-3）。
    ExpenseReport,
    /// 销售订单 `biz_sales_order`（销售链 · SO-05）。
    SaleOrder,
    /// 发货单 `biz_shipment`（销售链 · SO-09）。
    Shipment,
    /// 应收单 `biz_receivable`（销售链 · SO-13）。
    Receivable,
    /// 收款单 `biz_receipt`（销售链 · SO-17）。
    ///
    /// 资金单据的台账流（ARCH-7 §4.4）：`DRAFT → CONFIRMED → POSTED`，
    /// `DRAFT → CANCELLED` 作废。**状态域与应收 / 应付都不是一套值**。
    Receipt,
    /// 收款退款单 `biz_receipt_refund`（销售链 · SO-14 遗留卡）。
    ///
    /// 资金单据台账流（与收款同构）：`DRAFT → CONFIRMED → POSTED`，`DRAFT → CANCELLED`。
    /// **POSTED 是终态**（钱已付出且凭证已记账）。
    ReceiptRefund,
    /// 销售发票 `biz_invoice`（销售链 · SO-20）。
    ///
    /// 状态域（ARCH-7 §4.5）：`DRAFT → ISSUED → POSTED`，`DRAFT / ISSUED → VOID`。
    /// 开票（`ISSUED`）时生成收入凭证并回写应收，作废时红冲。
    Invoice,
    /// 销售退货单 `biz_sales_return`（销售链 · SO-29）。
    ///
    /// 状态域（ARCH-7 §15.2.4）：`DRAFT → SUBMITTED → APPROVED → PENDING_POST → POSTED`，
    /// `DRAFT / SUBMITTED / APPROVED → CANCELLED`。过账（回库 + 冲应收 + 红字凭证）
    /// 由 SO-30 在 `PENDING_POST → POSTED` 的同一事务里完成。
    SalesReturn,
    /// 采购退货单 `biz_purchase_return`（采购链 · P2-03）。
    ///
    /// 状态域：`DRAFT → SUBMITTED → APPROVED → CONFIRMED`，
    /// `DRAFT / SUBMITTED / APPROVED → CANCELLED`。确认（出库回冲 + 应付红字 +
    /// 供应商应退款 + 冲销凭证）在 `APPROVED → CONFIRMED` 的同一事务里完成。
    PurchaseReturn,
    /// 成本对象（生产批）`biz_cost_object`（成本核算 ARCH-9 §4.1）。
    ///
    /// 状态域三值 `OPEN`/`CLOSED`/`CANCELLED`，边表见 [`COST_OBJECT_EDGES`]。
    /// 与别的实体不同：它**不是审批单据**，状态变更全部来自成本核算侧
    /// （关批 = 不再领料报工），所以没有 `SUBMITTED` / `APPROVED` 这些中间态。
    CostObject,
    /// 成本核算批次（期间结算单）`biz_cost_period`（成本核算 ARCH-9 §4.1）。
    ///
    /// 状态域七值：`DRAFT`/`SUBMITTED`/`APPROVED`/`SETTLED`/`CLOSED`/`CANCELLED`/`REVERSED`，
    /// 边表见 [`COST_PERIOD_EDGES`]。卡面的 `PENDING` 就是这里的 `SUBMITTED`
    /// （V0373 的 `ck_biz_cost_period_status` 已按 `SUBMITTED` 落地）。
    CostPeriod,
    /// 员工档案 `biz_employee`（人力资源 ARCH-9 §4.2.1）。
    ///
    /// 状态域四值 `PROBATION`/`ACTIVE`/`SUSPENDED`/`RESIGNED`，边表见 [`EMPLOYEE_EDGES`]。
    /// 与 `sys_user.status`（0/1 账号启停）是两套口径：人事状态在员工档，
    /// 离职时两边都写（设计 D-13）。
    Employee,
    /// 劳动合同 `biz_employee_contract`（人力资源 ARCH-9 §3.1 ②）。
    ///
    /// 状态域四值 `ACTIVE`/`EXPIRED`/`TERMINATED`/`RENEWED`，边表见
    /// [`EMPLOYEE_CONTRACT_EDGES`]。续签是「旧合同封闭（`RENEWED`）+ 新合同接链
    /// （`renewed_from_id`）」，不是把旧合同改日期。
    EmployeeContract,
    /// 请假单 `biz_leave_request`（人力资源 ARCH-9 §4.2.2）。
    ///
    /// 状态域四值 `DRAFT`/`SUBMITTED`/`APPROVED`/`CANCELLED`，边表见 [`LEAVE_EDGES`]。
    /// 审批 `biz_type = HR_LEAVE`；草稿作废走软删，`CANCELLED` 只给「已批准的假要销掉」。
    LeaveRequest,
    /// 考勤期间（月度批次）`biz_attendance_period`（人力资源 ARCH-9 §4.2.3）。
    ///
    /// 状态域三值 `DRAFT`/`SUBMITTED`/`LOCKED`，边表见 [`ATTENDANCE_PERIOD_EDGES`]。
    /// **不是单据**（`DocKind::is_document()` 为 false）：锁定后是薪资试算的数据基线，
    /// 记录不许增删改。
    AttendancePeriod,
    /// 薪资单 `biz_salary_sheet`（人力资源 ARCH-9 §4.2.4）。
    ///
    /// 状态域**六值** `DRAFT`/`CALCULATED`/`SUBMITTED`/`APPROVED`/`POSTED`/`REVERSED`
    /// （V0383 的 `ck_biz_sal_sheet_status`，**无 `CANCELLED`**），边表见
    /// [`SALARY_SHEET_EDGES`]。过账出两张凭证（计提 + 发放），红冲出两张红字凭证。
    SalarySheet,
}

impl Entity {
    /// 全部实体（注册表全量，未知类型就是对表外的字符串）。
    pub const ALL: [Entity; 29] = [
        Entity::StockIn,
        Entity::PurchaseOrder,
        Entity::Transfer,
        Entity::Payable,
        Entity::Voucher,
        Entity::Period,
        Entity::YearEnd,
        Entity::Contract,
        Entity::StockOut,
        Entity::StockTake,
        Entity::Budget,
        Entity::FixedAsset,
        Entity::Depreciation,
        Entity::ExpenseReport,
        Entity::SaleOrder,
        Entity::Shipment,
        Entity::Receivable,
        Entity::Receipt,
        Entity::ReceiptRefund,
        Entity::Invoice,
        Entity::SalesReturn,
        Entity::PurchaseReturn,
        // 成本核算（ARCH-9）
        Entity::CostObject,
        Entity::CostPeriod,
        // 人力资源（ARCH-9-人力资源模块设计 §4.2）
        Entity::Employee,
        Entity::EmployeeContract,
        Entity::LeaveRequest,
        Entity::AttendancePeriod,
        Entity::SalarySheet,
    ];

    /// 落库的 `entity_type`。
    pub const fn as_str(self) -> &'static str {
        match self {
            Entity::StockIn => ENTITY_STOCK_IN,
            Entity::PurchaseOrder => ENTITY_PURCHASE_ORDER,
            Entity::Transfer => ENTITY_TRANSFER,
            Entity::Payable => ENTITY_PAYABLE,
            Entity::Voucher => ENTITY_VOUCHER,
            Entity::Period => ENTITY_PERIOD,
            Entity::YearEnd => ENTITY_YEAR_END,
            Entity::Contract => ENTITY_CONTRACT,
            Entity::StockOut => ENTITY_STOCK_OUT,
            Entity::StockTake => ENTITY_STOCK_TAKE,
            Entity::Budget => ENTITY_BUDGET,
            Entity::FixedAsset => ENTITY_FIXED_ASSET,
            Entity::Depreciation => ENTITY_DEPRECIATION,
            Entity::ExpenseReport => ENTITY_EXPENSE,
            Entity::SaleOrder => ENTITY_SALES_ORDER,
            Entity::Shipment => ENTITY_SHIPMENT,
            Entity::Receivable => ENTITY_RECEIVABLE,
            Entity::Receipt => ENTITY_RECEIPT,
            Entity::ReceiptRefund => ENTITY_RECEIPT_REFUND,
            Entity::Invoice => ENTITY_INVOICE,
            Entity::SalesReturn => ENTITY_SALES_RETURN,
            Entity::PurchaseReturn => ENTITY_PURCHASE_RETURN,
            Entity::CostObject => ENTITY_COST_OBJECT,
            Entity::CostPeriod => ENTITY_COST_PERIOD,
            Entity::Employee => ENTITY_EMPLOYEE,
            Entity::EmployeeContract => ENTITY_EMPLOYEE_CONTRACT,
            Entity::LeaveRequest => ENTITY_LEAVE_REQUEST,
            Entity::AttendancePeriod => ENTITY_ATTENDANCE_PERIOD,
            Entity::SalarySheet => ENTITY_SALARY_SHEET,
        }
    }

    /// 字符串 → 实体。**未知一律 `None`**（调用方必须报错，不许静默）。
    pub fn parse(raw: &str) -> Option<Entity> {
        Entity::ALL.into_iter().find(|item| item.as_str() == raw)
    }

    /// 中文名（错误文案 / 日志展示共用）。
    pub const fn label(self) -> &'static str {
        match self {
            Entity::StockIn => "收货入库单",
            Entity::PurchaseOrder => "采购订单",
            Entity::Transfer => "库存调拨单",
            Entity::Payable => "应付单",
            Entity::Voucher => "凭证",
            Entity::Period => "会计期间",
            Entity::YearEnd => "会计年度",
            Entity::Contract => "合同",
            Entity::StockOut => "出库单",
            Entity::StockTake => "盘点单",
            Entity::Budget => "预算",
            Entity::FixedAsset => "固定资产",
            Entity::Depreciation => "折旧批次",
            Entity::ExpenseReport => "费用报销单",
            Entity::SaleOrder => "销售订单",
            Entity::Shipment => "发货单",
            Entity::Receivable => "应收单",
            Entity::Receipt => "收款单",
            Entity::ReceiptRefund => "收款退款单",
            Entity::Invoice => "销售发票",
            Entity::SalesReturn => "销售退货单",
            Entity::PurchaseReturn => "采购退货单",
            Entity::CostObject => "成本对象",
            Entity::CostPeriod => "成本核算批次",
            Entity::Employee => "员工",
            Entity::EmployeeContract => "劳动合同",
            Entity::LeaveRequest => "请假单",
            Entity::AttendancePeriod => "考勤期间",
            Entity::SalarySheet => "薪资单",
        }
    }

    /// 合法边表（唯一真相）。
    pub const fn edges(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Entity::StockIn => &STOCK_IN_EDGES,
            Entity::PurchaseOrder => &PURCHASE_ORDER_EDGES,
            Entity::Transfer => &TRANSFER_EDGES,
            Entity::Payable => &PAYABLE_EDGES,
            Entity::Voucher => &VOUCHER_EDGES,
            Entity::Period => &PERIOD_EDGES,
            Entity::YearEnd => &YEAR_END_EDGES,
            Entity::Contract => &CONTRACT_EDGES,
            Entity::StockOut => &STOCK_OUT_EDGES,
            Entity::StockTake => &STOCK_TAKE_EDGES,
            Entity::Budget => &BUDGET_EDGES,
            Entity::FixedAsset => &FIXED_ASSET_EDGES,
            Entity::Depreciation => &DEPRECIATION_EDGES,
            Entity::ExpenseReport => &EXPENSE_EDGES,
            Entity::SaleOrder => &SALES_ORDER_EDGES,
            Entity::Shipment => &SHIPMENT_EDGES,
            Entity::Receivable => &RECEIVABLE_EDGES,
            Entity::Receipt => &RECEIPT_EDGES,
            Entity::ReceiptRefund => &RECEIPT_REFUND_EDGES,
            Entity::Invoice => &INVOICE_EDGES,
            Entity::SalesReturn => &SALES_RETURN_EDGES,
            Entity::PurchaseReturn => &PURCHASE_RETURN_EDGES,
            Entity::CostObject => &COST_OBJECT_EDGES,
            Entity::CostPeriod => &COST_PERIOD_EDGES,
            Entity::Employee => &EMPLOYEE_EDGES,
            Entity::EmployeeContract => &EMPLOYEE_CONTRACT_EDGES,
            Entity::LeaveRequest => &LEAVE_EDGES,
            Entity::AttendancePeriod => &ATTENDANCE_PERIOD_EDGES,
            Entity::SalarySheet => &SALARY_SHEET_EDGES,
        }
    }

    /// 硬约束白名单（第 1 层）：`(from, to) + mandatory / default_label / requires_reason`。
    ///
    /// 与 [`Entity::edges`] **逐条对应**（顺序也一致），单测 `base_edges_match_registry`
    /// 钉死这件事——两张表不能漂移。
    pub const fn base_edges(self) -> &'static [BaseEdge] {
        match self {
            Entity::StockIn => &STOCK_IN_BASE,
            Entity::PurchaseOrder => &PURCHASE_ORDER_BASE,
            Entity::Transfer => &TRANSFER_BASE,
            Entity::Payable => &PAYABLE_BASE,
            Entity::Voucher => &VOUCHER_BASE,
            Entity::Period => &PERIOD_BASE,
            Entity::YearEnd => &YEAR_END_BASE,
            Entity::Contract => &CONTRACT_BASE,
            Entity::StockOut => &STOCK_OUT_BASE,
            Entity::StockTake => &STOCK_TAKE_BASE,
            Entity::Budget => &BUDGET_BASE,
            Entity::FixedAsset => &FIXED_ASSET_BASE,
            Entity::Depreciation => &DEPRECIATION_BASE,
            Entity::ExpenseReport => &EXPENSE_BASE,
            Entity::SaleOrder => &SALES_ORDER_BASE,
            Entity::Shipment => &SHIPMENT_BASE,
            Entity::Receivable => &RECEIVABLE_BASE,
            Entity::Receipt => &RECEIPT_BASE,
            Entity::ReceiptRefund => &RECEIPT_REFUND_BASE,
            Entity::Invoice => &INVOICE_BASE,
            Entity::SalesReturn => &SALES_RETURN_BASE,
            Entity::PurchaseReturn => &PURCHASE_RETURN_BASE,
            Entity::CostObject => &COST_OBJECT_BASE,
            Entity::CostPeriod => &COST_PERIOD_BASE,
            Entity::Employee => &EMPLOYEE_BASE,
            Entity::EmployeeContract => &EMPLOYEE_CONTRACT_BASE,
            Entity::LeaveRequest => &LEAVE_BASE,
            Entity::AttendancePeriod => &ATTENDANCE_PERIOD_BASE,
            Entity::SalarySheet => &SALARY_SHEET_BASE,
        }
    }

    /// 按 `(from, to)` 找 BASE 边（找不到 = 这条边在代码里就不存在）。
    pub fn base_edge(self, from: &str, to: &str) -> Option<&'static BaseEdge> {
        self.base_edges().iter().find(|edge| edge.from == from && edge.to == to)
    }

    /// 需要 reason 的边。
    pub const fn reason_edges(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Entity::StockIn => &STOCK_IN_REASON_EDGES,
            Entity::PurchaseOrder => &PURCHASE_ORDER_REASON_EDGES,
            Entity::Transfer => &TRANSFER_REASON_EDGES,
            Entity::Payable => &PAYABLE_REASON_EDGES,
            Entity::Voucher => &VOUCHER_REASON_EDGES,
            Entity::Period => &PERIOD_REASON_EDGES,
            Entity::YearEnd => &YEAR_END_REASON_EDGES,
            Entity::Contract => &CONTRACT_REASON_EDGES,
            Entity::StockOut => &STOCK_OUT_REASON_EDGES,
            Entity::StockTake => &STOCK_TAKE_REASON_EDGES,
            Entity::Budget => &BUDGET_REASON_EDGES,
            Entity::FixedAsset => &FIXED_ASSET_REASON_EDGES,
            Entity::Depreciation => &DEPRECIATION_REASON_EDGES,
            Entity::ExpenseReport => &EXPENSE_REASON_EDGES,
            Entity::SaleOrder => &SALES_ORDER_REASON_EDGES,
            Entity::Shipment => &SHIPMENT_REASON_EDGES,
            Entity::Receivable => &RECEIVABLE_REASON_EDGES,
            Entity::Receipt => &RECEIPT_REASON_EDGES,
            Entity::ReceiptRefund => &RECEIPT_REFUND_REASON_EDGES,
            Entity::Invoice => &INVOICE_REASON_EDGES,
            Entity::SalesReturn => &SALES_RETURN_REASON_EDGES,
            Entity::PurchaseReturn => &PURCHASE_RETURN_REASON_EDGES,
            Entity::CostObject => &COST_OBJECT_REASON_EDGES,
            Entity::CostPeriod => &COST_PERIOD_REASON_EDGES,
            Entity::Employee => &EMPLOYEE_REASON_EDGES,
            Entity::EmployeeContract => &EMPLOYEE_CONTRACT_REASON_EDGES,
            Entity::LeaveRequest => &LEAVE_REASON_EDGES,
            Entity::AttendancePeriod => &ATTENDANCE_PERIOD_REASON_EDGES,
            Entity::SalarySheet => &SALARY_SHEET_REASON_EDGES,
        }
    }

    /// 入账态（只读）。
    pub const fn locked_statuses(self) -> &'static [&'static str] {
        match self {
            Entity::StockIn => &STOCK_IN_LOCKED,
            Entity::PurchaseOrder => &PURCHASE_ORDER_LOCKED,
            Entity::Transfer => &TRANSFER_LOCKED,
            Entity::Payable => &PAYABLE_LOCKED,
            Entity::Voucher => &VOUCHER_LOCKED,
            Entity::Period => &PERIOD_LOCKED,
            Entity::YearEnd => &YEAR_END_LOCKED,
            Entity::Contract => &CONTRACT_LOCKED,
            Entity::StockOut => &STOCK_OUT_LOCKED,
            Entity::StockTake => &STOCK_TAKE_LOCKED,
            Entity::Budget => &BUDGET_LOCKED,
            Entity::FixedAsset => &FIXED_ASSET_LOCKED,
            Entity::Depreciation => &DEPRECIATION_LOCKED,
            Entity::ExpenseReport => &EXPENSE_LOCKED,
            Entity::SaleOrder => &SALES_ORDER_LOCKED,
            Entity::Shipment => &SHIPMENT_LOCKED,
            Entity::Receivable => &RECEIVABLE_LOCKED,
            Entity::Receipt => &RECEIPT_LOCKED,
            Entity::ReceiptRefund => &RECEIPT_REFUND_LOCKED,
            Entity::Invoice => &INVOICE_LOCKED,
            Entity::SalesReturn => &SALES_RETURN_LOCKED,
            Entity::PurchaseReturn => &PURCHASE_RETURN_LOCKED,
            Entity::CostObject => &COST_OBJECT_LOCKED,
            Entity::CostPeriod => &COST_PERIOD_LOCKED,
            Entity::Employee => &EMPLOYEE_LOCKED,
            Entity::EmployeeContract => &EMPLOYEE_CONTRACT_LOCKED,
            Entity::LeaveRequest => &LEAVE_LOCKED,
            Entity::AttendancePeriod => &ATTENDANCE_PERIOD_LOCKED,
            Entity::SalarySheet => &SALARY_SHEET_LOCKED,
        }
    }

    /// 某状态能去的下一个状态（空 = 终态）。
    pub fn next_statuses(self, from: &str) -> Vec<&'static str> {
        self.edges()
            .iter()
            .filter(|(f, _)| *f == from)
            .map(|(_, t)| *t)
            .collect()
    }

    /// 合法边判定。
    pub fn can_transition(self, from: &str, to: &str) -> bool {
        self.edges().iter().any(|(f, t)| *f == from && *t == to)
    }

    /// 这条边是否必须有 reason。
    pub fn needs_reason(self, from: &str, to: &str) -> bool {
        self.reason_edges().iter().any(|(f, t)| *f == from && *t == to)
    }

    /// 该状态是否入账态。
    pub fn is_locked(self, status: &str) -> bool {
        self.locked_statuses().contains(&status)
    }
}

// ==================== 第 2 / 3 层：配置表与运行时合并 ====================
//
// 【D1 流程配置化 · 阶段 3】
//
// ```text
// 第 2 层  biz_flow_state_edge（V0204）—— 只能从 BASE_EDGES 挑子集，且只能加严
// 第 3 层  effective_edges()         —— 运行时合并；读失败 / 表不存在 / 数据脏 → 退化 BASE_EDGES
// ```
//
// # 允许的配置动作（写入口 `upsert_edge_config` 逐条校验）
//
// | 动作 | 怎么表达 | 校验 |
// |------|----------|------|
// | 禁用一条**可选**边 | `enabled = false` | BASE 边必须是 `mandatory = false`，否则 400 |
// | 给某条边加「必填原因」 | `requires_reason = true` | 只能加不能减（BASE 要求原因的边关不掉） |
// | 改显示文案 | `label = "报批"` | ≤ 64 字，空 = 用 BASE 的 default_label |
// | 追加权限码 | `auth_code = "erp:stock-in:post"` | ≤ 64 字，只能加 |
//
// # 禁止的配置动作（代码里硬拒，不靠自觉）
//
// - 添加 BASE_EDGES 里没有的边 → 400（真相在代码里，配置只是挑子集）
// - 禁用 `mandatory = true` 的边 → 400；万一有人绕过 API 直接写库，加载时**忽略 + WARN**
// - 改边的方向 / 任意跳任意 → 400（`(from,to)` 必须在 BASE 里，方向自然钉死）
//
// # 留痕
//
// 每次写入（含删除）都往 `biz_status_log` 追加一行（`entity_type = FLOW_EDGE_CFG`，
// 只增不改）：谁、什么时候、把哪条边从什么改成什么（改前 / 改后快照都进 `reason`）。
// 写操作要求 **SUPER_ADMIN + 显式二次确认**（`confirm = true`），
// 「改流程规则」不能是个手滑就能点的动作。
//
// # 为什么没有进程内缓存
//
// 每次流转都读一次配置（一张只按 entity 索引的小表，几十行）。好处是**没有生效时延**、
// 也没有「缓存里是旧配置」的中间态：删掉配置下一笔流转立刻回到 BASE_EDGES 行为。
// 读到脏数据 / 读失败只有一种结果——退回 BASE_EDGES 并打 WARN（60 秒内同一条只打一次，
// 避免把日志刷爆）。

/// 配置留痕用的 `biz_status_log.entity_type`（它不是单据，单独一个类型名）。
pub const ENTITY_EDGE_CONFIG: &str = DocKind::FlowEdgeConfig.as_str();

/// 合并后的一条有效边。
///
/// **只可能来自 BASE_EDGES**（配置不能创造新边），`label` / `requires_reason` / `auth_code`
/// 可能被配置加严；`mandatory` 直接沿用 BASE 的标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// 变更前状态。
    pub from: &'static str,
    /// 变更后状态。
    pub to: &'static str,
    /// 是否硬约束边（配置禁用无效）。
    pub mandatory: bool,
    /// 显示文案（配置可覆盖，默认来自 BASE 的 `default_label`）。
    pub label: String,
    /// 走这条边是否必须给原因（BASE 要求 or 配置要求，取「或」）。
    pub requires_reason: bool,
    /// 追加的权限码要求（空 = 不追加）。
    pub auth_code: Option<String>,
}

impl Edge {
    /// BASE 边 → 有效边（没有任何配置时的形态）。
    fn from_base(base: &BaseEdge) -> Self {
        Self {
            from: base.from,
            to: base.to,
            mandatory: base.meta.mandatory,
            label: base.meta.default_label.to_string(),
            requires_reason: base.meta.requires_reason,
            auth_code: None,
        }
    }
}

/// `biz_flow_state_edge` 的一行（**存活行**，`deleted_at IS NULL`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeConfigRow {
    /// 主键。
    pub id: Uuid,
    /// 实体类型（落库原文，可能写错——写错的会在合并时被忽略并 WARN）。
    pub entity: String,
    /// 变更前状态。
    pub from_status: String,
    /// 变更后状态。
    pub to_status: String,
    /// 是否启用（false = 禁用这条可选边）。
    pub enabled: bool,
    /// 是否要求原因。
    pub requires_reason: bool,
    /// 显示文案（空 = 用 BASE 的 default_label）。
    pub label: Option<String>,
    /// 追加的权限码。
    pub auth_code: Option<String>,
}

impl EdgeConfigRow {
    /// 行 → 结构体。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            entity: row.get_typed("entity")?,
            from_status: row.get_typed("from_status")?,
            to_status: row.get_typed("to_status")?,
            enabled: row.get_typed("enabled")?,
            requires_reason: row.get_typed("requires_reason")?,
            label: row.get_typed::<Option<String>>("label")?,
            auth_code: row.get_typed::<Option<String>>("auth_code")?,
        })
    }
}

/// 合并结果：有效边 + 本次发现的配置问题（WARN 文案，测试直接断言它）。
#[derive(Debug, Clone, Default)]
pub struct EdgeResolution {
    /// 有效边（顺序与 BASE_EDGES 一致）。
    pub edges: Vec<Edge>,
    /// 配置问题（脏数据 / 违规禁用 / 读失败）。非空即代表「这份配置没被完整采纳」。
    pub warnings: Vec<String>,
}

impl EdgeResolution {
    /// 这条边当前可用吗。
    pub fn allows(&self, from: &str, to: &str) -> bool {
        self.get(from, to).is_some()
    }

    /// 取一条有效边。
    pub fn get(&self, from: &str, to: &str) -> Option<&Edge> {
        self.edges.iter().find(|edge| edge.from == from && edge.to == to)
    }

    /// 某状态当前能去的下一个状态（配置禁用过的不会出现在这里）。
    pub fn next_statuses(&self, from: &str) -> Vec<&'static str> {
        self.edges
            .iter()
            .filter(|edge| edge.from == from)
            .map(|edge| edge.to)
            .collect()
    }
}

/// 纯 BASE 边表（没有任何配置 / 配置整体作废时的形态）。
fn base_only(entity: Entity) -> Vec<Edge> {
    entity.base_edges().iter().map(Edge::from_base).collect()
}

/// **纯函数**：`BASE_EDGES ⊕ 配置行 → 有效边`（不做 IO，单测逐条覆盖合并规则）。
///
/// 规则（与卡面 §3 一一对应）：
/// 1. 配置行 `entity` 不在注册表里 → 整组忽略 + WARN（**配置脏要退化，不能让系统崩**）。
/// 2. BASE 边 + 同 `(from,to)` 且 `enabled = true` → 采用配置的 label / requires_reason / auth_code。
/// 3. BASE 边 + 同 `(from,to)` 且 `enabled = false`：
///    - `mandatory = false` → **这条边从有效边里去掉**（合法禁用）；
///    - `mandatory = true`  → 忽略禁用 + WARN（硬约束白名单不可删）。
/// 4. 配置里有、BASE 里没有的 `(from,to)` → 忽略 + WARN（配置不能创造新边）。
/// 5. 合并后任何一个 `from` 状态**没有任何出边** → 整份配置作废、退回 BASE_EDGES + WARN
///    （只许加严，不许把单据卡死）。
pub fn resolve_edges(entity: Entity, rows: &[EdgeConfigRow]) -> EdgeResolution {
    let mut warnings: Vec<String> = Vec::new();

    // ① entity 写错的行：先报出来（它是「配置脏」最典型的样子）
    let mut unknown_entities: Vec<&str> = Vec::new();
    for row in rows {
        let raw = row.entity.trim();
        if Entity::parse(raw).is_none() && !unknown_entities.contains(&raw) {
            unknown_entities.push(raw);
        }
    }
    for raw in unknown_entities {
        let pairs: Vec<String> = rows
            .iter()
            .filter(|row| row.entity.trim() == raw)
            .map(|row| format!("{}→{}", row.from_status.trim(), row.to_status.trim()))
            .collect();
        warnings.push(format!(
            "配置脏：entity「{raw}」不在状态机注册表里（{} 条：{}），整组忽略（已退化到 BASE_EDGES）",
            pairs.len(),
            pairs.join(" / ")
        ));
    }

    // ② 本实体的配置按 (from,to) 建索引（唯一约束本该拦住重复，兜底取第一行）
    let mut by_key: HashMap<(&str, &str), &EdgeConfigRow> = HashMap::new();
    for row in rows {
        if Entity::parse(row.entity.trim()) != Some(entity) {
            continue;
        }
        let key = (row.from_status.trim(), row.to_status.trim());
        if by_key.insert(key, row).is_some() {
            warnings.push(format!(
                "配置脏：{} 的 {}→{} 有多行配置（唯一约束本该拦住），只采用第一行",
                entity.as_str(),
                key.0,
                key.1
            ));
        }
    }

    // ③ 逐条 BASE 边合并
    let mut edges: Vec<Edge> = Vec::with_capacity(entity.base_edges().len());
    for base in entity.base_edges() {
        let Some(row) = by_key.get(&base.key()) else {
            edges.push(Edge::from_base(base));
            continue;
        };
        if !row.enabled {
            if base.meta.mandatory {
                warnings.push(format!(
                    "配置试图禁用 mandatory 边 {}→{}（硬约束白名单：主链 / 审批落地 / 撤销回位 / 红冲），\
                     已忽略：该边仍然可用",
                    base.from, base.to
                ));
            } else {
                // 合法禁用：这条边不出现在有效边里
                continue;
            }
        }
        let label = row
            .label
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| base.meta.default_label.to_string());
        let auth_code = row
            .auth_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        edges.push(Edge {
            from: base.from,
            to: base.to,
            mandatory: base.meta.mandatory,
            label,
            // 只能加严：BASE 要求原因的边，配置写成 false 也照样要求
            requires_reason: base.meta.requires_reason || row.requires_reason,
            auth_code,
        });
    }

    // ④ 配置里有、BASE 里没有的边：忽略 + WARN
    for key in by_key.keys() {
        if entity.base_edge(key.0, key.1).is_none() {
            warnings.push(format!(
                "配置脏：{} 的 {}→{} 不在 BASE_EDGES 里（配置不能创造新边，也不能改方向），已忽略",
                entity.as_str(),
                key.0,
                key.1
            ));
        }
    }

    // ⑤ 停摆护栏：任何一个 from 状态都不能没有出口
    for base in entity.base_edges() {
        if !edges.iter().any(|edge| edge.from == base.from) {
            warnings.push(format!(
                "配置会让状态 {} 失去所有出口（单据会永久卡死：只许加严，不许停摆）\
                 → 整份配置作废，退回 BASE_EDGES",
                base.from
            ));
            return EdgeResolution {
                edges: base_only(entity),
                warnings,
            };
        }
    }

    EdgeResolution { edges, warnings }
}

/// 读**全部**存活配置行（不按实体过滤：要能发现 `entity` 写错这类脏行）。
///
/// # 为什么包在 SAVEPOINT 里（真机踩到的坑）
///
/// 这一步是 fail-safe 的读：表不存在 / 列不匹配 / 权限不足都要**退化**而不是抛错。
/// 但 `transition()` 的正常调用方都开着事务（`Transaction`），而 PG 里**一条语句报错
/// 会把整个事务置为 aborted**（后续语句一律 `current transaction is aborted`）——
/// 于是「读配置失败」会让紧接着的 `insert_log` 500：本该降级的路径反而把业务写死。
/// 真机验收（把配置表改名）实测到的就是 `写状态变更日志失败 … query failed`。
///
/// 所以：在事务里（`post_commit_queue().is_some()`）先 `SAVEPOINT`，读完按结果
/// `RELEASE` 或 `ROLLBACK TO`；自动提交模式（`Db`）没有事务可污染，直接读。
pub async fn load_edge_config_rows(ex: &impl SqlExecutor) -> DbResult<Vec<EdgeConfigRow>> {
    const SAVEPOINT: &str = "d1_edge_config_read";
    let in_tx = ex.post_commit_queue().is_some();
    if in_tx {
        ex.exec_sql(format!("SAVEPOINT {SAVEPOINT}"), vec![]).await?;
    }
    let result = ex
        .query_sql(
            "SELECT id, entity, from_status, to_status, enabled, requires_reason, label, auth_code \
               FROM biz_flow_state_edge WHERE deleted_at IS NULL \
              ORDER BY entity ASC, from_status ASC, to_status ASC"
                .to_string(),
            vec![],
        )
        .await;
    if in_tx {
        // 收尾语句本身失败不该盖掉主结果（此时事务状态由上层决定）
        let tail = match &result {
            Ok(_) => format!("RELEASE SAVEPOINT {SAVEPOINT}"),
            Err(_) => format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}"),
        };
        let _ = ex.exec_sql(tail, vec![]).await;
    }
    let rows = result?;
    rows.iter().map(EdgeConfigRow::from_row).collect()
}

/// 运行时合并（**fail-safe**）：读配置失败 / 表不存在 / 数据脏 → 退回 BASE_EDGES + WARN。
///
/// 这个函数**永远不会返回 Err**：状态机是业务主链路，配置读不到只能降级，不能让单据流转崩。
pub async fn edge_resolution(ex: &impl SqlExecutor, entity: Entity) -> EdgeResolution {
    match load_edge_config_rows(ex).await {
        Ok(rows) => resolve_edges(entity, &rows),
        Err(error) => EdgeResolution {
            edges: base_only(entity),
            warnings: vec![format!(
                "读取 biz_flow_state_edge 失败（{error}）：已退化到 BASE_EDGES（状态机照旧可用，配置本次不生效）"
            )],
        },
    }
}

/// 有效边（业务代码唯一该用的形态）：`BASE_EDGES ∩ 配置`，读失败退化到 BASE_EDGES。
pub async fn effective_edges(ex: &impl SqlExecutor, entity: Entity) -> Vec<Edge> {
    let resolution = edge_resolution(ex, entity).await;
    log_edge_warnings(entity, &resolution.warnings);
    resolution.edges
}

/// 配置告警落日志（**60 秒内同一条只打一次**：流转是高频路径，别把日志刷爆）。
fn log_edge_warnings(entity: Entity, warnings: &[String]) {
    for warning in warnings {
        warn_edge_throttled(entity.as_str(), warning);
    }
}

fn warn_edge_throttled(scope: &str, detail: &str) {
    static SEEN: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{scope}|{detail}");
    let now = Instant::now();
    // 锁中毒不能影响业务：拿 `into_inner` 继续用（这里只是日志节流表）
    let mut guard = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = guard.get(&key)
        && now.duration_since(*last) < Duration::from_secs(60)
    {
        return;
    }
    if guard.len() > 512 {
        guard.clear();
    }
    guard.insert(key, now);
    tracing::warn!(
        context = "state_machine.edge_config",
        entity = scope,
        detail = detail,
        "状态机边配置告警（同一条 60 秒内只打一次）"
    );
}

/// 改配置的入参。
///
/// `confirm` 是**二次确认**：改状态机配置 = 改流程规则，必须显式传 `true`
/// （UI 上就是那个「我确认要改流程规则」的勾选框），否则 400。
///
/// `confirm_critical` 是**禁用主链路边时的第三次确认**（【E2E-FIX-1】）：
/// 只有当 `enabled = false` 且这条边在 [`CRITICAL_EDGES`] 里时才需要传 `true`，
/// 否则 400 并点名「禁用它会让{impact}」。别的组合传了也没用（不做任何放宽）。
#[derive(Debug, Clone)]
pub struct EdgeConfigPatch {
    /// 实体类型（`STOCK_IN` / `PURCHASE_ORDER` / …）。
    pub entity: String,
    /// 变更前状态。
    pub from_status: String,
    /// 变更后状态。
    pub to_status: String,
    /// 是否启用（false = 禁用这条可选边）。
    pub enabled: bool,
    /// 是否要求原因（只能加不能减）。
    pub requires_reason: bool,
    /// 显示文案（空 = 用 BASE 的 default_label）。
    pub label: Option<String>,
    /// 追加的权限码。
    pub auth_code: Option<String>,
    /// 二次确认（必须 true）。
    pub confirm: bool,
    /// 禁用「主链路必需边」的确认（`enabled = false` 且 `critical_edge(...)` 命中时才需要）。
    pub confirm_critical: bool,
}

impl EdgeConfigPatch {
    /// 「启用这条边 + 不改加严项」的最简形态。
    pub fn allow(entity: &str, from_status: &str, to_status: &str) -> Self {
        Self {
            entity: entity.to_string(),
            from_status: from_status.to_string(),
            to_status: to_status.to_string(),
            enabled: true,
            requires_reason: false,
            label: None,
            auth_code: None,
            confirm: true,
            confirm_critical: false,
        }
    }

    /// 链式：标记「我已确认这是主链路必需边，仍要禁用」（只影响禁用的那一次校验）。
    pub fn with_confirm_critical(mut self, confirm: bool) -> Self {
        self.confirm_critical = confirm;
        self
    }
}

/// 超管判定（与系统约定一致：`SUPER_ADMIN` 角色 或 通配权限）。
fn is_super_admin(claims: &TokenClaims) -> bool {
    claims.is_super_admin()
        || claims
            .roles
            .iter()
            .any(|role| role == crate::shared::constants::SUPER_ADMIN_ROLE_CODE)
}

/// 改配置的门：**只有超管**，且要二次确认。
fn ensure_config_admin(claims: &TokenClaims, action: &str) -> Result<(), ApiError> {
    if is_super_admin(claims) {
        Ok(())
    } else {
        Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            format!(
                "{action}状态机配置需要超级管理员（SUPER_ADMIN）：状态机的边就是流程规则，\
                 能改它的人等于能放宽全公司的审批路径，只能由超管在「系统管理」里操作，\
                 不能自助开启"
            ),
        ))
    }
}

/// 校验 + 规整一个可空文本配置项（空白视作未配置，超长 400）。
fn normalize_config_text(raw: Option<&String>, field: &str, max: usize) -> Result<Option<String>, ApiError> {
    let Some(text) = raw.map(|value| value.trim()).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > max {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 最长 {max} 个字符（当前 {}）", text.chars().count()),
        ));
    }
    Ok(Some(text.to_string()))
}

/// 取一行配置（改前快照用）。
async fn fetch_edge_config(
    ex: &impl SqlExecutor,
    entity: &str,
    from: &str,
    to: &str,
) -> DbResult<Option<EdgeConfigRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, entity, from_status, to_status, enabled, requires_reason, label, auth_code \
               FROM biz_flow_state_edge \
              WHERE entity = ? AND from_status = ? AND to_status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(entity.to_string()),
                Value::Text(from.to_string()),
                Value::Text(to.to_string()),
            ],
        )
        .await?;
    rows.first().map(EdgeConfigRow::from_row).transpose()
}

/// 改配置的一行快照描述（写进审计 `reason`，人能一眼看出改了啥）。
fn describe_config(row: Option<&EdgeConfigRow>) -> String {
    match row {
        None => "无配置（使用 BASE_EDGES）".to_string(),
        Some(row) => format!(
            "enabled={} requires_reason={} label={} auth_code={}",
            row.enabled,
            row.requires_reason,
            row.label.as_deref().unwrap_or("(BASE)"),
            row.auth_code.as_deref().unwrap_or("(无)")
        ),
    }
}

/// 写一行配置审计（`biz_status_log`，只增不改）。
#[allow(clippy::too_many_arguments)]
async fn write_config_audit(
    ex: &impl SqlExecutor,
    claims: &TokenClaims,
    entity: Entity,
    from: &str,
    to: &str,
    entity_id: Uuid,
    from_state: &str,
    to_state: &str,
    before: Option<&EdgeConfigRow>,
    after: Option<&EdgeConfigRow>,
) -> Result<(), ApiError> {
    let entity_no = format!("{} {from}>{to}", entity.as_str());
    let reason = format!(
        "改前 {{{}}} → 改后 {{{}}}",
        describe_config(before),
        describe_config(after)
    );
    insert_log(
        ex,
        ENTITY_EDGE_CONFIG,
        entity_id,
        &entity_no,
        Some(from_state),
        to_state,
        &Operator::from(claims),
        Some(&reason),
        None,
        None,
    )
    .await
    .map(|_| ())
}

/// **改状态机配置的唯一入口**：超管 + 二次确认 + 只能加严 + 写审计。
///
/// # 错误
///
/// - 非超管 → **403**
/// - `confirm != true` / 未知实体 / BASE 里没有这条边 / 禁用 mandatory 边 /
///   试图把「代码要求原因」的边放宽 → **400**
pub async fn upsert_edge_config(
    ex: &impl SqlExecutor,
    claims: &TokenClaims,
    patch: &EdgeConfigPatch,
) -> Result<EdgeConfigRow, ApiError> {
    ensure_config_admin(claims, "修改")?;
    if !patch.confirm {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "改状态机配置需要二次确认（confirm = true）：这条边就是流程规则，\
             改它等于改全公司的审批路径"
                .to_string(),
        ));
    }
    let entity = Entity::parse(patch.entity.trim()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "未知的单据类型「{}」：状态机只认 {}",
                patch.entity.trim(),
                Entity::ALL
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        )
    })?;
    let from = patch.from_status.trim();
    let to = patch.to_status.trim();
    let base = entity.base_edge(from, to).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} 的 {from}→{to} 不在 BASE_EDGES 里：状态机配置只能从代码里的合法边挑子集，\
                 不能创造新边、也不能改方向（允许的边：{}）",
                entity.as_str(),
                entity
                    .base_edges()
                    .iter()
                    .map(|edge| format!("{}→{}", edge.from, edge.to))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        )
    })?;
    if !patch.enabled && base.meta.mandatory {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} 的 {from}→{to} 是硬约束边（mandatory，默认文案「{}」），不能禁用：\
                 禁掉它会把单据卡死（审批回调 500 / 账不可纠正）。可禁用的是 optional 边（捷径边）",
                entity.as_str(),
                base.meta.default_label
            ),
        ));
    }
    if base.meta.requires_reason && !patch.requires_reason {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} 的 {from}→{to} 在代码里就要求填原因，配置不能把它放宽（只许加严）",
                entity.as_str()
            ),
        ));
    }
    // 【E2E-FIX-1】主链路必需边：禁用要**第三次确认**（`confirm` 只是「我要改配置」，
    // 这里问的是「你知道关掉它会让哪个功能直接做不了吗」）。
    // 2026-11-09 E2E-02 演练：`STOCK_OUT DRAFT→POSTED` 被悄悄关掉 → 发货过账 409 → 整条销售链断，
    // 而改配置的人根本没意识到自己在关主链路。报文必须**点名业务后果**，不能只说「这是一条边」。
    if !patch.enabled
        && let Some(critical) = critical_edge(entity, from, to)
        && !patch.confirm_critical
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "这是主链路必需边（{} {from}→{to}），禁用它会让{}。\
                 确认要禁用请传 confirm_critical=true（一般不需要禁用主链路边，\
                 请先确认是不是点错了对象）",
                entity.as_str(),
                critical.impact
            ),
        ));
    }
    let label = normalize_config_text(patch.label.as_ref(), "label", 64)?;
    let auth_code = normalize_config_text(patch.auth_code.as_ref(), "auth_code", 64)?;

    let before = fetch_edge_config(ex, entity.as_str(), from, to)
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读状态机配置失败（改配置前必须留改前快照）：{error}"),
            )
        })?;

    let id: Uuid = ex
        .query_first_sql(
            "INSERT INTO biz_flow_state_edge \
                 (id, entity, from_status, to_status, enabled, requires_reason, label, auth_code, \
                  created_at, updated_at, create_by, update_by, deleted_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULLIF(?::text,''), NULLIF(?::text,''), ?, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, NULL) \
             ON CONFLICT (entity, from_status, to_status) DO UPDATE \
                SET enabled = EXCLUDED.enabled, \
                    requires_reason = EXCLUDED.requires_reason, \
                    label = EXCLUDED.label, \
                    auth_code = EXCLUDED.auth_code, \
                    updated_at = EXCLUDED.updated_at, \
                    update_by = EXCLUDED.update_by, \
                    deleted_at = NULL \
             RETURNING id"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(entity.as_str().to_string()),
                Value::Text(from.to_string()),
                Value::Text(to.to_string()),
                Value::Bool(patch.enabled),
                Value::Bool(patch.requires_reason),
                Value::Text(label.clone().unwrap_or_default()),
                Value::Text(auth_code.clone().unwrap_or_default()),
                Value::from(Utc::now()),
                Value::from(Utc::now()),
                Value::Text(
                    crate::utils::current_user_uuid(claims)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(
                    crate::utils::current_user_uuid(claims)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
            ],
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("写状态机配置失败（{} {from}→{to}）：{error}", entity.as_str()),
            )
        })?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "写状态机配置没有返回主键（upsert 异常）".to_string(),
            )
        })?
        .get_typed("id")
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读状态机配置主键失败：{error}"),
            )
        })?;

    let after = EdgeConfigRow {
        id,
        entity: entity.as_str().to_string(),
        from_status: from.to_string(),
        to_status: to.to_string(),
        enabled: patch.enabled,
        requires_reason: patch.requires_reason,
        label,
        auth_code,
    };
    write_config_audit(
        ex,
        claims,
        entity,
        from,
        to,
        id,
        if before.as_ref().is_some_and(|row| !row.enabled) {
            "DISABLED"
        } else if before.is_some() {
            "ENABLED"
        } else {
            "ABSENT"
        },
        if patch.enabled { "ENABLED" } else { "DISABLED" },
        before.as_ref(),
        Some(&after),
    )
    .await?;
    Ok(after)
}

/// 删掉一条配置（软删，语义是「回到 BASE_EDGES 行为」）：超管 + 二次确认 + 写审计。
///
/// 返回 `true` = 确实删掉了一行；`false` = 本来就没有配置（**不是错误**，
/// 重复删除是幂等的，审计也不会记一条空动作）。
pub async fn delete_edge_config(
    ex: &impl SqlExecutor,
    claims: &TokenClaims,
    entity_raw: &str,
    from_status: &str,
    to_status: &str,
) -> Result<bool, ApiError> {
    ensure_config_admin(claims, "删除")?;
    let entity = Entity::parse(entity_raw.trim()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("未知的单据类型「{}」", entity_raw.trim()),
        )
    })?;
    let from = from_status.trim();
    let to = to_status.trim();
    if entity.base_edge(from, to).is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} 的 {from}→{to} 不在 BASE_EDGES 里：它本来就不是一条合法边，没有配置可删",
                entity.as_str()
            ),
        ));
    }
    let before = fetch_edge_config(ex, entity.as_str(), from, to)
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读状态机配置失败（删除前必须留改前快照）：{error}"),
            )
        })?;
    let Some(before) = before else {
        return Ok(false);
    };
    let affected = ex
        .exec_sql(
            "UPDATE biz_flow_state_edge \
                SET deleted_at = now(), updated_at = now(), \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE entity = ? AND from_status = ? AND to_status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(
                    crate::utils::current_user_uuid(claims)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(entity.as_str().to_string()),
                Value::Text(from.to_string()),
                Value::Text(to.to_string()),
            ],
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("删除状态机配置失败（{} {from}→{to}）：{error}", entity.as_str()),
            )
        })?;
    if affected == 0 {
        return Ok(false);
    }
    write_config_audit(
        ex,
        claims,
        entity,
        from,
        to,
        before.id,
        if before.enabled { "ENABLED" } else { "DISABLED" },
        "DELETED",
        Some(&before),
        None,
    )
    .await?;
    Ok(true)
}

/// 列出现有配置（管理端 / 排障用；`entity = None` 表示全量）。
pub async fn list_edge_config(ex: &impl SqlExecutor, entity: Option<Entity>) -> DbResult<Vec<EdgeConfigRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, entity, from_status, to_status, enabled, requires_reason, label, auth_code \
               FROM biz_flow_state_edge \
              WHERE deleted_at IS NULL AND (?::text = '' OR entity = ?) \
              ORDER BY entity ASC, from_status ASC, to_status ASC"
                .to_string(),
            vec![
                Value::Text(entity.map(|item| item.as_str()).unwrap_or_default().to_string()),
                Value::Text(entity.map(|item| item.as_str()).unwrap_or_default().to_string()),
            ],
        )
        .await?;
    rows.iter().map(EdgeConfigRow::from_row).collect()
}

// ==================== 操作人与返回 ====================

/// 令牌声明里的权限快照（**只有 HTTP 入口有**；内部 / 后台路径没有）。
///
/// 它只服务一件事：`biz_flow_state_edge.auth_code` 配了权限码时，
/// 判断「当前操作人有没有这个码」。`None` 表示调用方不是令牌持有者
/// （审批回调 / 定时任务 / 内部编排），此时**不拦**、只打 WARN ——
/// 拦下来会让审批回调 500（那是停摆，不是加严）。
#[derive(Debug, Clone, Default)]
pub struct AuthSnapshot {
    /// 角色码（`SUPER_ADMIN` 短路一切）。
    pub roles: Vec<String>,
    /// 权限码（`*` / `*:*:*` 视作通配）。
    pub permissions: Vec<String>,
}

impl AuthSnapshot {
    /// 从令牌声明取快照。
    pub fn from_claims(claims: &TokenClaims) -> Self {
        Self {
            roles: claims.roles.clone(),
            permissions: claims.permissions.clone(),
        }
    }

    /// 超管（`SUPER_ADMIN` 角色 或 通配权限码）。
    pub fn is_super_admin(&self) -> bool {
        self.roles
            .iter()
            .any(|role| role == crate::shared::constants::SUPER_ADMIN_ROLE_CODE)
            || self.permissions.iter().any(|code| code == "*" || code == "*:*:*")
    }

    /// 持有某个权限码（超管短路）。
    pub fn has_permission(&self, code: &str) -> bool {
        self.is_super_admin() || self.permissions.iter().any(|held| held == code)
    }
}

/// 操作人（`operator` 入参的统一形态）。
///
/// 卡面写的是 `&Principal`，但仓里没有这个类型：handler 拿到的是框架的
/// [`TokenClaims`]，而 model / 事务体只带 `Option<Uuid>`。为了让**两边都能直接传**，
/// 这里收一个 `impl Into<Operator>`：`None` / `Some(uuid)` / `&TokenClaims` 都行。
#[derive(Debug, Clone, Default)]
pub struct Operator {
    /// 用户 id（落 `operator_id`）。
    pub id: Option<Uuid>,
    /// 用户名（落 `operator_name`：日志的可读性靠它，权威关联仍靠 id）。
    pub name: Option<String>,
    /// 令牌里的权限快照（`&TokenClaims` 传入时自动带上；内部路径为 `None`）。
    pub auth: Option<AuthSnapshot>,
}

impl Operator {
    /// 只有 id（model / 事务体路径）。
    pub fn of(id: Option<Uuid>) -> Self {
        Self {
            id,
            name: None,
            auth: None,
        }
    }

    /// id + 用户名。
    pub fn named(id: Option<Uuid>, name: impl Into<String>) -> Self {
        let name = name.into();
        let name = name.trim();
        Self {
            id,
            name: if name.is_empty() { None } else { Some(name.to_string()) },
            auth: None,
        }
    }
}

impl From<Option<Uuid>> for Operator {
    fn from(value: Option<Uuid>) -> Self {
        Operator::of(value)
    }
}

impl From<Uuid> for Operator {
    fn from(value: Uuid) -> Self {
        Operator::of(Some(value))
    }
}

impl From<&TokenClaims> for Operator {
    fn from(claims: &TokenClaims) -> Self {
        Operator {
            id: crate::utils::current_user_uuid(claims),
            name: claims
                .username
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            auth: Some(AuthSnapshot::from_claims(claims)),
        }
    }
}

/// 一次状态变更的返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    /// 变更前状态。
    pub from: String,
    /// 变更后状态。
    pub to: String,
    /// `biz_status_log` 的主键（调用方 trace / 断言用）。
    pub log_id: Uuid,
}

/// `biz_status_log` 的一行（轨迹查询）。
#[derive(Debug, Clone)]
pub struct StatusLogRow {
    /// 主键（与 [`TransitionOutcome::log_id`] 对应）。
    pub id: Uuid,
    /// 单据类型。
    pub entity_type: String,
    /// 单据 id。
    pub entity_id: Uuid,
    /// 单号。
    pub entity_no: String,
    /// 变更前（NULL = 建单）。
    pub from_status: Option<String>,
    /// 变更后。
    pub to_status: String,
    /// 业务动作码（E2E05-FIX-3 ①）：`None` = 真正的状态流转（`from → to`）；
    /// `Some(TRANSFER)` / `Some(CUSTODIAN)` = 状态**没变**的业务动作（部门转移 / 使用人变更）。
    /// 没有这一列时，任何按 `from → to` 渲染时间线的消费者只能把这类行读成
    /// 「IN_USE → IN_USE」，看起来像脏数据（V0365 加的列）。
    pub action: Option<String>,
    /// 操作人 id。
    pub operator_id: Option<Uuid>,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 变更原因。
    pub reason: Option<String>,
    /// 关联单据类型。
    pub biz_ref_type: Option<String>,
    /// 关联单号。
    pub biz_ref_no: Option<String>,
    /// 写入时间。
    pub created_at: DateTime<Utc>,
}

// ==================== 唯一入口 ====================

/// 未知实体类型：**明确报错**，不静默放过（卡面单测 ⑤）。
fn unknown_entity(entity_type: &str) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!(
            "未知的单据类型 `{entity_type}`：状态机只认 {}（shared/state_machine.rs 的实体注册表），\
             新实体必须先在注册表登记合法边 / 入账态，再用 transition() 改状态",
            Entity::ALL
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        ),
    )
}

/// 非法流转的 409 文案（点名「当前态的可选去向」）。
pub fn rejection_message(entity_type: &str, from: &str, to: &str) -> String {
    let Some(entity) = Entity::parse(entity_type) else {
        return format!("单据类型 {entity_type} 未在状态机注册，不能流转到 {to}");
    };
    rejection_message_for(entity, from, to, &entity.next_statuses(from))
}

/// 同上，但「可选去向」由调用方给（配置禁用后要按**有效边**报，不能报已禁用的边）。
fn rejection_message_for(entity: Entity, from: &str, to: &str, allowed: &[&'static str]) -> String {
    if allowed.is_empty() {
        return format!(
            "{}当前是「{from}」，没有可流转的下一个状态：它已是终态，或该状态由其它流程维护",
            entity.label()
        );
    }
    format!(
        "{}当前是「{from}」，不能直接流转到「{to}」：允许的去向是 {}",
        entity.label(),
        allowed
            .iter()
            .map(|status| format!("「{status}」"))
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

/// 权限码闸：`biz_flow_state_edge.auth_code` 非空时，操作人必须持有该码。
///
/// - 超管（`SUPER_ADMIN` 角色 / 通配权限）短路放行；
/// - 令牌入口（HTTP）没有该码 → **403**；
/// - 内部路径（审批回调 / 定时任务，没有令牌快照）→ 放行 + WARN
///   （拦下来会让审批回调 500，那是停摆，不是加严）。
fn check_edge_auth(entity: Entity, edge: &Edge, operator: &Operator) -> Result<(), ApiError> {
    let Some(code) = edge.auth_code.as_deref() else {
        return Ok(());
    };
    match operator.auth.as_ref() {
        Some(auth) if auth.has_permission(code) => Ok(()),
        Some(_) => Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            format!(
                "{}的「{}→{}」需要权限码 `{code}`（当前账号未持有）：\
                 这条边被管理员收紧过，只有持有该权限的人能走",
                entity.label(),
                edge.from,
                edge.to
            ),
        )),
        None => {
            warn_edge_throttled(
                entity.as_str(),
                &format!(
                    "{}→{} 配了权限码 `{code}`，但本次调用没有令牌（内部路径），未校验权限码",
                    edge.from, edge.to
                ),
            );
            Ok(())
        }
    }
}

/// 注册表核对：未知实体 / 边表与注册表不一致 → **500**（配置漂移，不是用户错）。
fn check_registration(entity_type: &str, edges: &[(&str, &str)]) -> Result<Entity, ApiError> {
    let entity = Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    if edges != entity.edges() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!(
                "{entity_type} 的边表与状态机注册表不一致：{} 传了 {} 条，注册表是 {} 条。\
                 改边表必须改 shared/state_machine.rs 的注册表（唯一真相），不能在调用点私自放宽",
                entity_type,
                edges.len(),
                entity.edges().len()
            ),
        ));
    }
    Ok(entity)
}

/// **唯一状态变更入口**：校验边 → 校验 reason → 写 `biz_status_log`。
///
/// 调用方必须在**已经开着的事务**里调用，并在返回后继续做副作用
/// （加库存 / 生成凭证 / 改状态本身）；任一步失败整笔回滚，日志不会半条落地。
///
/// # 参数
///
/// * `entity_type` —— `STOCK_IN` / `PURCHASE_ORDER` / `TRANSFER` / `PAYABLE` / `VOUCHER`
/// * `edges` —— 调用方 model 里的 `LEGAL_EDGES`（会与注册表核对，防「偷偷多放一条边」）
/// * `biz_ref` —— 触发本次变更的关联单据 `(类型, 单号)`，如 `("STOCK_IN", "IN-…")`
///
/// # 错误
///
/// * 未知 `entity_type` / 边表与注册表不一致 / 单号为空 → **500**（配置漂移，不是用户错）
/// * 非法边（含自环）→ **409** + 当前态可选去向
/// * 需要 reason 的边缺 reason → **400**
#[allow(clippy::too_many_arguments)]
pub async fn transition(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    edges: &[(&str, &str)],
    from: &str,
    to: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<TransitionOutcome, ApiError> {
    let entity = check_registration(entity_type, edges)?;
    let no = entity_no.trim();
    if no.is_empty() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{entity_type} 的状态变更缺少单号：状态日志没有单号就查不到轨迹"),
        ));
    }
    if from == to {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "{} {} 的状态没有变化（{from} → {to}）：原地流转不是状态变更，\
                 不要写状态日志（重复部分收货请只写业务流水）",
                entity.label(),
                no
            ),
        ));
    }

    // 【D1】第 3 层：有效边 = BASE_EDGES ∩ 配置（读配置失败 / 数据脏 → 自动退化 BASE_EDGES + WARN）。
    // 边表核对（`check_registration`）用的是**代码里的注册表**，所以「谁私自多放一条边」
    // 依然当场 500；配置层只能在这里**再收窄**，永远不能放宽。
    let operator = operator.into();
    let resolution = edge_resolution(ex, entity).await;
    log_edge_warnings(entity, &resolution.warnings);
    let Some(edge) = resolution.get(from, to) else {
        // 【E2E-FIX-1】配置层禁用的边：给用户看的是**业务语言**，不是内部单号 + 状态名。
        // 「出库单 OUT-20261109-000031 的流转 DRAFT → POSTED 已被状态机配置禁用」这种报文
        // 摆在销售用户面前等于没说 —— 单号是内部单号，状态名是状态机术语，他能做的只有一件事：
        // 找系统管理员。单号 / 状态 / 当前可选去向一律只进 `internal_detail`（只写日志）。
        if entity.can_transition(from, to) {
            let impact = match critical_edge(entity, from, to) {
                Some(critical) => critical.impact.to_string(),
                None => match entity.base_edge(from, to) {
                    Some(base) => format!("{}无法{}", entity.label(), base.meta.default_label),
                    None => format!("{}无法继续这一步操作", entity.label()),
                },
            };
            let mut error = ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("{impact}：该操作已被系统配置禁用，请联系系统管理员"),
            );
            // `internal_detail` 只写日志、不返回前端（内部单号 / 状态名 / 可选去向都在这里）
            error.internal_detail = Some(format!(
                "{entity_type} {no} 的流转 {from} → {to} 被 biz_flow_state_edge 禁用（关键边={}）；\
                 当前允许去向：{:?}",
                critical_edge(entity, from, to).is_some(),
                resolution.next_statuses(from)
            ));
            return Err(error);
        }
        let message = rejection_message_for(entity, from, to, &resolution.next_statuses(from));
        return Err(ApiError::new(409, error_code::RESOURCE_CONFLICT, message));
    };

    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    // 必填原因 = BASE 要求 or 配置要求（合并时取的「或」，只能加不能减）
    if edge.requires_reason && reason.is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} {} 从 {from} 流转到 {to} 必须给出原因（reason）：\
                 该动作会影响已入账的数据或已生效的承诺，原因要能追溯到为什么",
                entity.label(),
                no
            ),
        ));
    }

    // 配置层追加的权限码要求（令牌入口；内部路径放行 + WARN）
    check_edge_auth(entity, edge, &operator)?;

    let log_id = insert_log(
        ex,
        entity_type,
        entity_id,
        no,
        Some(from),
        to,
        &operator,
        reason,
        biz_ref,
        None,
    )
    .await?;
    Ok(TransitionOutcome {
        from: from.to_string(),
        to: to.to_string(),
        log_id,
    })
}

/// **「状态真的变了才写日志」的入口**：业务性推进（部分付款 / 继续部分收货）用它。
///
/// | `from` vs `to` | 行为 |
/// |---|---|
/// | 不同 | 等价于 [`transition`]，写一行 `biz_status_log`，返回 `Ok(Some(outcome))` |
/// | 相同（原地流转） | **不写日志**，返回 `Ok(None)` —— 这是「没变」而不是「失败」 |
///
/// # 为什么不让 [`transition`] 自己放过自环（LNK-P08 的教训）
///
/// `from == to` 的 409 是 P10 铁律③的刹车：`transition` 是**五个实体共用的唯一入口**，
/// 放宽它会让所有「调用点写错了」的回归都变成静默的空日志。
/// 但「同一状态下的第二次业务动作」是**正常业务**（应付第二期部分付款
/// `PARTIAL_PAID → PARTIAL_PAID`、采购单继续部分收货），它本来就不该产生状态日志。
///
/// 所以「允许原地不动」这件事必须由**调用点显式声明**（用本函数而不是 [`transition`]），
/// 语义一目了然：`Ok(None)` = 状态没变、只该留业务流水。
///
/// 注册表核对（未知实体 / 边表漂移 500）在自环短路**之前**做——
/// 否则调用点的实体名写错会被自环静默吞掉。
// 与 [`transition`] 同款签名（同一批入参 + 同一套校验），参数数量由状态机协议决定；
// 拆结构体会让两个入口的调用点写法不一致，反而更容易写错。
#[allow(clippy::too_many_arguments)]
pub async fn transition_if_changed(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    edges: &[(&str, &str)],
    from: &str,
    to: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<Option<TransitionOutcome>, ApiError> {
    check_registration(entity_type, edges)?;
    if from == to {
        return Ok(None);
    }
    let outcome = transition(
        ex,
        entity_type,
        entity_id,
        entity_no,
        edges,
        from,
        to,
        operator,
        reason,
        biz_ref,
    )
    .await?;
    Ok(Some(outcome))
}

/// 建单留痕（`from_status = NULL` → `to_status`）。
///
/// 建单本身不是「状态流转」（没有 from），所以单独一个入口，写同构的一行日志，
/// 让「null→DRAFT」「null→OPEN」「null→POSTED」也能在轨迹里看到。
pub async fn log_created(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    to_status: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
) -> Result<Uuid, ApiError> {
    Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    let no = entity_no.trim();
    if no.is_empty() || to_status.trim().is_empty() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{entity_type} 的建单日志缺单号或目标状态（单号 / 状态是轨迹的唯一线索）"),
        ));
    }
    let operator = operator.into();
    insert_log(
        ex,
        entity_type,
        entity_id,
        no,
        None,
        to_status,
        &operator,
        reason.map(str::trim).filter(|text| !text.is_empty()),
        None,
        None,
    )
    .await
}

/// 写一行「状态机配置变更」的审计备注（`biz_status_log`，`entity_type = 'FLOW_EDGE_CFG'`）。
///
/// # 为什么需要它（D3 审批化）
///
/// 配置行本身的变化由 [`upsert_edge_config`] / [`delete_edge_config`] 留痕（改前 → 改后快照）。
/// 但审批化之后还有几种**不是「行变了」**的事件也得能被审计回答：
///
/// | 事件 | 谁写 | state |
/// |------|------|-------|
/// | 没有已发布流程时「显式跳过审批」直接生效 | `flow::edge_admin` | `SKIPPED` |
/// | 审批通过、配置已生效 | `flow::edge_admin`（回调） | `APPLIED` |
/// | 审批驳回 / 撤回（配置一分没动） | `flow::edge_admin`（回调） | `REJECTED` |
/// | 审批通过但没落地（发起人已不是超管 / 内核拒绝） | `flow::edge_admin`（回调） | `APPROVED` |
///
/// 这里只做 INSERT（与 [`insert_log`] 同一个写入形态），**不碰任何配置行** ——
/// 唯一写入口仍然是 `upsert_edge_config` / `delete_edge_config`。
#[allow(clippy::too_many_arguments)]
pub async fn note_edge_config(
    ex: &impl SqlExecutor,
    entity: Entity,
    from: &str,
    to: &str,
    entity_id: Uuid,
    from_state: &str,
    to_state: &str,
    operator: Operator,
    reason: &str,
) -> Result<(), ApiError> {
    insert_log(
        ex,
        ENTITY_EDGE_CONFIG,
        entity_id,
        &format!("{} {from}>{to}", entity.as_str()),
        Some(from_state),
        to_state,
        &operator,
        Some(reason),
        None,
        None,
    )
    .await
    .map(|_| ())
}

/// 写一条「状态没变」的业务留痕（转移 / 使用人变更这类不改变状态的变动）。
///
/// # 为什么需要它
///
/// ARCH-6 §5.6.1 / §5.6.2 要求部门转移与使用人变更**也写状态日志**
/// （`entity_type='FIXED_ASSET'`，`reason` = 变动原因），但这两件事**不改变状态**——
/// 走 [`transition`] 会被「原地流转不是状态变更」的护栏挡住（那条护栏是对的：
/// 状态没变就不是状态变更）。
///
/// 所以这里显式区分两种留痕：
/// * `from_status ≠ to_status` → 状态变更，必须走 [`transition`]（校验边 + 写日志）；
/// * `from_status = to_status` → 业务事件，走本函数（**只写日志**，不校验边）。
///
/// 这一条 `from = to` 的日志在时间线上读起来是「6 月 3 日 由 研发部 转到 产品部」——
/// 正是内控想看到的；它不会被 `state_machine::can_transition` 认成一条边。
///
/// # `action`：把「业务动作」和「状态流转」在数据层分开（E2E05-FIX-3 ①）
///
/// `from_status = to_status` 的行光看状态两列是「IN_USE → IN_USE」，任何按
/// `from → to` 渲染时间线的消费者都只能把它当脏数据。`action` 是这些事件的
/// **动作码**（`TRANSFER` / `CUSTODIAN`，与 `biz_fixed_asset_change.change_type`
/// 同字面），前端据此显示「部门转移」「使用人变更」；`None` = 未分类的业务事件。
/// 真正的状态流转由 [`transition`] 写，`action` 恒为 `NULL`。
// 与 `transition` 同一组参数（实体 + 单据 + 状态 + 操作人 + 原因 + 关联单据），
// 多出来的 `action` 只描述「这是哪种业务动作」，不参与状态判定。
#[allow(clippy::too_many_arguments)]
pub async fn log_event(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    status: &str,
    action: Option<&str>,
    operator: impl Into<Operator>,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<Uuid, ApiError> {
    let entity = Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    let no = entity_no.trim();
    if no.is_empty() || status.trim().is_empty() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{entity_type} 的业务留痕缺少单号或状态：日志没有单号就查不到轨迹"),
        ));
    }
    insert_log(
        ex,
        entity.as_str(),
        entity_id,
        no,
        Some(status),
        status,
        &operator.into(),
        reason,
        biz_ref,
        action,
    )
    .await
}

/// 写一行 `biz_status_log`（**只有 INSERT，没有 UPDATE**）。
#[allow(clippy::too_many_arguments)]
async fn insert_log(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    from_status: Option<&str>,
    to_status: &str,
    operator: &Operator,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
    action: Option<&str>,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let (ref_type, ref_no) = match biz_ref {
        Some((kind, no)) => (kind.trim().to_string(), no.trim().to_string()),
        None => (String::new(), String::new()),
    };
    // 操作人姓名快照（FIX-9 ③）：调用方手里常常只有一个 `Uuid`
    // （`Operator::of(Some(id))` —— model / 事务体路径拿不到令牌），以前 `operator_name`
    // 就这样一路写空，用户改名 / 销号之后审计只剩一串 uuid。
    // 这里在**没有名字时**补一次主键查询：优先 `real_name`，没填就退回 `username`。
    // 查不到（历史脏数据 / 号已硬删）→ 留空，**不阻断**状态变更（日志是旁路，不该把业务卡死）。
    let operator_name = match operator.name.as_deref() {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => match operator.id {
            None => String::new(),
            Some(user_id) => {
                // SCOPE-EXEMPT: 按主键查单个用户的名字快照（登录主体已知），不产出用户可见行集。
                let resolved = ex
                    .query_first_sql(
                        "SELECT COALESCE(NULLIF(btrim(real_name), ''), username) AS name \
                           FROM sys_user WHERE id = ?::uuid"
                            .to_string(),
                        vec![Value::from(&user_id)],
                    )
                    .await
                    .map_err(|error| {
                        ApiError::new(
                            500,
                            error_code::INTERNAL_ERROR,
                            format!("写状态变更日志失败（{entity_type} {entity_no}）：查操作人姓名 {error}"),
                        )
                    })?;
                match resolved {
                    Some(row) => row
                        .get_typed::<Option<String>>("name")
                        .unwrap_or_default()
                        .unwrap_or_default(),
                    None => String::new(),
                }
            }
        },
    };
    ex.exec_sql(
        "INSERT INTO biz_status_log \
            (id, entity_type, entity_id, entity_no, from_status, to_status, \
             operator_id, operator_name, reason, biz_ref_type, biz_ref_no, created_at, action) \
         VALUES (?, ?, ?::uuid, ?, NULLIF(?::text,''), ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
                 NULLIF(?::text,''), ?, NULLIF(?::text,''))"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(entity_type.to_string()),
            Value::from(&entity_id),
            Value::Text(entity_no.to_string()),
            Value::Text(from_status.unwrap_or_default().to_string()),
            Value::Text(to_status.to_string()),
            Value::Text(operator.id.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(operator_name),
            Value::Text(reason.unwrap_or_default().to_string()),
            Value::Text(ref_type),
            Value::Text(ref_no),
            Value::from(Utc::now()),
            Value::Text(action.unwrap_or_default().to_string()),
        ],
    )
    .await
    .map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("写状态变更日志失败（{entity_type} {entity_no} → {to_status}）：{error}"),
        )
    })?;
    Ok(id)
}

// ==================== 入账后只读 ====================

/// 入账态只读的 409 文案。
pub fn locked_message(entity_type: &str, status: &str, entity_no: &str) -> String {
    let label = Entity::parse(entity_type).map(|item| item.label()).unwrap_or("单据");
    format!(
        "单据已入账（状态 {status}），金额/数量/明细不可修改。\n 如需更正请先作废或红冲后重开。\
         （{label} {entity_no}；白名单：remark / status / updated_at / update_by / 系统回写字段）"
    )
}

/// 入账态只读闸：**各 `update_*` 入口必须调**（不靠前端隐藏按钮）。
///
/// 未登记的实体类型同样 500 —— 新单据忘了登记就等于没有只读保护，必须早失败。
pub fn ensure_writable(entity_type: &str, status: &str, entity_no: &str) -> Result<(), ApiError> {
    let entity = Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    if entity.is_locked(status) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            locked_message(entity_type, status, entity_no),
        ));
    }
    Ok(())
}

// ==================== 轨迹查询 ====================

/// 按单号读完整轨迹（时间正序）。
///
/// 详情页「状态历史」与 e2e 断言共用；`entity_no` 是人对得上的唯一标识，
/// 单据软删 / 硬删之后日志仍然查得到。
pub async fn trail_of_no(ex: &impl SqlExecutor, entity_no: &str) -> DbResult<Vec<StatusLogRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, entity_type, entity_id, entity_no, from_status, to_status, action, \
                    operator_id, operator_name, reason, biz_ref_type, biz_ref_no, created_at \
               FROM biz_status_log WHERE entity_no = ? ORDER BY created_at ASC, id ASC"
                .to_string(),
            vec![Value::Text(entity_no.to_string())],
        )
        .await?;
    rows.iter().map(StatusLogRow::from_row).collect()
}

impl StatusLogRow {
    /// 行 → 结构体（可空列一律 `get_typed::<Option<_>>`）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            entity_type: row.get_typed("entity_type")?,
            entity_id: row.get_typed("entity_id")?,
            entity_no: row.get_typed("entity_no")?,
            from_status: row.get_typed::<Option<String>>("from_status")?,
            to_status: row.get_typed("to_status")?,
            action: row.get_typed::<Option<String>>("action")?,
            operator_id: row.get_typed::<Option<Uuid>>("operator_id")?,
            operator_name: row.get_typed::<Option<String>>("operator_name")?,
            reason: row.get_typed::<Option<String>>("reason")?,
            biz_ref_type: row.get_typed::<Option<String>>("biz_ref_type")?,
            biz_ref_no: row.get_typed::<Option<String>>("biz_ref_no")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

// ==================== 单测（真库）====================
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0159 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// export ERP_DB_URL='postgres://p10role:p10pass@127.0.0.1:5432/cloudpivot_p10'
// cargo test --lib business::shared::state_machine
// ```
//
// 夹具：`entity_no` 一律 `P10SM-` 前缀，跑前跑后按前缀清；`biz_status_log` 没有外键，
// 所以不需要造真实单据行 —— 这也正好证明「日志比业务行活得久」。
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    const PREFIX: &str = "P10SM-";

    /// 连库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    async fn cleanup() {
        db().execute_with_params("DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SM-%'", vec![])
            .await
            .unwrap_or_else(|err| panic!("清理状态日志失败：{err}"));
        db().execute_with_params("DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SMV-%'", vec![])
            .await
            .unwrap();
    }

    fn op() -> Operator {
        Operator::named(
            Some(Uuid::from_str("00000000-0000-4000-8000-0000000000a1").unwrap()),
            "p10_tester",
        )
    }

    async fn count_of(entity_no: &str) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_status_log WHERE entity_no = ?",
            vec![Value::Text(entity_no.to_string())],
        )
        .await
        .unwrap()
    }

    /// ① 合法边通过，写一行字段完整的 `biz_status_log`。
    #[tokio::test]
    async fn legal_edge_writes_complete_log_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}A");
        let outcome = transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            Some((ENTITY_STOCK_IN, "IN-20261031-000001")),
        )
        .await
        .unwrap();
        assert_eq!(outcome.from, "DRAFT");
        assert_eq!(outcome.to, "POSTED");

        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 1);
        let row = &trail[0];
        assert_eq!(row.entity_type, ENTITY_STOCK_IN);
        assert_eq!(row.entity_id, id);
        assert_eq!(row.entity_no, no);
        assert_eq!(row.from_status.as_deref(), Some("DRAFT"));
        assert_eq!(row.to_status, "POSTED");
        assert_eq!(row.operator_name.as_deref(), Some("p10_tester"));
        assert!(row.operator_id.is_some(), "操作人 id 必须落库");
        assert_eq!(row.biz_ref_type.as_deref(), Some(ENTITY_STOCK_IN));
        assert_eq!(row.biz_ref_no.as_deref(), Some("IN-20261031-000001"));
        assert_eq!(row.id, outcome.log_id, "返回的 log_id 必须是真落库的那一行");
    }

    /// ② 非法边 → 409，文案点名当前态可选去向。
    #[tokio::test]
    async fn illegal_edge_is_conflict_and_names_targets() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}B"),
            &STOCK_IN_EDGES,
            "POSTED",
            "DRAFT",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("POSTED"), "{}", err.msg);
        assert!(err.msg.contains("CANCELLED"), "可选去向要点名：{}", err.msg);
        assert_eq!(count_of(&format!("{PREFIX}B")).await, 0, "非法边不写日志");
    }

    /// ③ 需要 reason 的边缺 reason → 400。
    #[tokio::test]
    async fn reason_required_edge_without_reason_is_bad_request() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}C"),
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("reason"), "{}", err.msg);
        assert_eq!(count_of(&format!("{PREFIX}C")).await, 0);

        // 给了 reason 就放行
        transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}C"),
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            Some("客户退货"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(count_of(&format!("{PREFIX}C")).await, 1);
    }

    /// ④ 同一单据连续两次流转 → 两行日志，时间顺序正确。
    #[tokio::test]
    async fn two_transitions_write_two_ordered_rows() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}D");
        transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap();
        transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            Some("红冲重开"),
            None,
        )
        .await
        .unwrap();

        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].to_status, "POSTED");
        assert_eq!(trail[1].to_status, "CANCELLED");
        assert_eq!(trail[1].from_status.as_deref(), Some("POSTED"));
        assert!(
            trail[0].created_at <= trail[1].created_at,
            "日志必须能按时间还原顺序：{:?} / {:?}",
            trail[0].created_at,
            trail[1].created_at
        );
    }

    /// ⑤ 未知 entity_type → 明确报错（不静默）。
    #[tokio::test]
    async fn unknown_entity_type_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            "NOT_A_DOCUMENT",
            Uuid::now_v7(),
            &format!("{PREFIX}E"),
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500, "{}", err.msg);
        assert!(err.msg.contains("未知的单据类型"), "{}", err.msg);
        assert!(err.msg.contains(ENTITY_PURCHASE_ORDER), "要点名注册表：{}", err.msg);
    }

    /// ⑥ 按 entity_no 能查完整轨迹（多单据互不串）。
    #[tokio::test]
    async fn trail_by_entity_no_is_complete() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no_a = format!("{PREFIX}F1");
        let no_b = format!("{PREFIX}F2");
        for (no, to) in [(&no_a, "SUBMITTED"), (&no_a, "CLOSED"), (&no_b, "SUBMITTED")] {
            let from = if to == "CLOSED" { "SUBMITTED" } else { "DRAFT" };
            transition(
                db(),
                ENTITY_PURCHASE_ORDER,
                Uuid::now_v7(),
                no,
                &PURCHASE_ORDER_EDGES,
                from,
                to,
                op(),
                None,
                None,
            )
            .await
            .unwrap();
        }
        let trail_a = trail_of_no(db(), &no_a).await.unwrap();
        assert_eq!(trail_a.len(), 2);
        assert_eq!(trail_a[0].from_status.as_deref(), Some("DRAFT"));
        assert_eq!(trail_a[1].to_status, "CLOSED");
        let trail_b = trail_of_no(db(), &no_b).await.unwrap();
        assert_eq!(trail_b.len(), 1, "别的单号不能串进来");
    }

    /// ⑦ 事务内失败 → 日志一起回滚（不写半条）。
    #[tokio::test]
    async fn log_rolls_back_with_transaction() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}G");
        let tx = db().begin().await.unwrap();
        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap();
        // 事务内可见
        let inside: i64 = tx
            .scalar_sql(
                "SELECT count(*) FROM biz_status_log WHERE entity_no = ?".to_string(),
                vec![Value::Text(no.clone())],
            )
            .await
            .unwrap();
        assert_eq!(inside, 1, "事务内必须能看到自己写的日志");
        tx.rollback().await.unwrap();
        assert_eq!(count_of(&no).await, 0, "回滚后不能留下半条日志");
    }

    /// ⑯ 非法边 `DRAFT → RECEIVED`（采购订单）→ 409。
    #[tokio::test]
    async fn purchase_order_draft_to_received_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_PURCHASE_ORDER,
            Uuid::now_v7(),
            &format!("{PREFIX}H"),
            &PURCHASE_ORDER_EDGES,
            "DRAFT",
            "RECEIVED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("DRAFT") && err.msg.contains("RECEIVED"), "{}", err.msg);
        assert_eq!(count_of(&format!("{PREFIX}H")).await, 0);
    }

    /// ⑰ 自环护栏**不放宽**：`transition` 收到 `from == to` 仍然 409（LNK-P08 修的是调用点，
    /// 不是护栏）。这条用例是护栏的回归锁 —— 谁想「顺手放宽 state_machine」会先看到它红。
    #[tokio::test]
    async fn self_loop_is_still_rejected_by_transition() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}I");
        let err = transition(
            db(),
            ENTITY_PAYABLE,
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("状态没有变化"), "{}", err.msg);
        assert_eq!(count_of(&no).await, 0, "自环绝不能写日志");
    }

    /// ⑱ `transition_if_changed` 原地流转 → `Ok(None)` + **零日志**（分期付款第二期）。
    ///
    /// 同一位置也要挡住「实体名写错」：注册表核对在自环短路**之前**做，
    /// 否则调用点把 `PAYABLE` 写成 `PAYABLES` 会被自环静默吞掉。
    #[tokio::test]
    async fn transition_if_changed_skips_self_loop() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}J");
        let outcome = transition_if_changed(
            db(),
            ENTITY_PAYABLE,
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            Some("第二期部分付款"),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_none(), "状态没变必须返回 Ok(None)");
        assert_eq!(count_of(&no).await, 0, "原地流转不许写状态日志");

        // 实体名写错 → 500，不被自环短路掩盖
        let err = transition_if_changed(
            db(),
            "PAYABLES",
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500, "{}", err.msg);
        assert!(err.msg.contains("未知的单据类型"), "{}", err.msg);
    }

    /// ⑲ `transition_if_changed` 真变化 → `Ok(Some(..))`，日志与 `transition` 同款落库。
    #[tokio::test]
    async fn transition_if_changed_writes_when_changed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}K");
        let outcome = transition_if_changed(
            db(),
            ENTITY_PAYABLE,
            id,
            &no,
            &PAYABLE_EDGES,
            "OPEN",
            "PARTIAL_PAID",
            op(),
            Some("首期部分付款"),
            Some((ENTITY_PAYABLE, "PM-P10SM-000001")),
        )
        .await
        .unwrap()
        .expect("状态真的变了，必须返回 Some");
        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].id, outcome.log_id);
        assert_eq!(trail[0].from_status.as_deref(), Some("OPEN"));
        assert_eq!(trail[0].to_status, "PARTIAL_PAID");
        assert_eq!(trail[0].biz_ref_no.as_deref(), Some("PM-P10SM-000001"));
    }

    /// ⑫ POSTED 凭证改摘要 → 409（入账后只读，凭证侧）。
    #[tokio::test]
    async fn posted_voucher_summary_change_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 直接造一张 POSTED 凭证（本用例只验只读闸，不验分录平衡）
        let id = Uuid::now_v7();
        let text = id.to_string();
        let no = format!("P10SMV-{}", &text[text.len() - 8..]);
        db().execute_with_params(
            "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 voucher_type, source_type, summary, total_debit, total_credit, entry_count, \
                 status, is_red) \
             VALUES (?::uuid, '记', ?, '2026-10', CURRENT_DATE, 'AUTO', 'STOCK_IN', '测试摘要', \
                 0, 0, 2, 'POSTED', 0)",
            vec![Value::from(&id), Value::Text(no.clone())],
        )
        .await
        .unwrap();

        let err = crate::business::erp::voucher_write::update_summary(id, Some("改过的摘要"), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已入账"), "{}", err.msg);
        let summary: String = db()
            .scalar_with_params(
                "SELECT summary FROM biz_voucher WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(summary, "测试摘要", "409 时不能真改库");

        db().execute_with_params("DELETE FROM biz_voucher WHERE id = ?::uuid", vec![Value::from(&id)])
            .await
            .unwrap();
    }
    // ==================== 【D1】第 1 层：BASE_EDGES ====================

    /// ❶ 纯重构不变量：BASE_EDGES 与 `edges()` **逐条同序**，`requires_reason` 与
    /// `reason_edges()` 完全一致，无自环、无重复、文案非空。
    ///
    /// 这条是「抽 BASE_EDGES 时行为一丝不变」的机器化证明：两张表漂移立刻红。
    #[test]
    fn base_edges_match_registry() {
        for entity in Entity::ALL {
            let base = entity.base_edges();
            let edges = entity.edges();
            assert_eq!(
                base.len(),
                edges.len(),
                "{:?} 的 BASE_EDGES 条数与 edges() 不一致",
                entity
            );
            for (index, (pair, base_edge)) in edges.iter().zip(base.iter()).enumerate() {
                assert_eq!(
                    (base_edge.from, base_edge.to),
                    *pair,
                    "{:?} 第 {index} 条边不一致（BASE 与注册表必须逐条同序）",
                    entity
                );
                assert_ne!(base_edge.from, base_edge.to, "自环不是状态变更：{pair:?}");
                assert!(
                    !base_edge.meta.default_label.trim().is_empty(),
                    "{pair:?} 缺默认文案（配置层的「恢复默认」要靠它）"
                );
                let reason_expected = entity
                    .reason_edges()
                    .iter()
                    .any(|(from, to)| *from == base_edge.from && *to == base_edge.to);
                assert_eq!(
                    base_edge.meta.requires_reason, reason_expected,
                    "{pair:?} 的 requires_reason 与 reason_edges() 不一致"
                );
            }
            // 无重复 (from,to)
            for (index, first) in base.iter().enumerate() {
                for second in base.iter().skip(index + 1) {
                    assert_ne!(
                        (first.from, first.to),
                        (second.from, second.to),
                        "{:?} 有重复边 {first:?}",
                        entity
                    );
                }
            }
        }
    }

    /// ❷ 可选边白名单钉死：**只有这 4 条**能被配置禁用，多一条都要先改这张断言。
    #[test]
    fn optional_edges_are_exactly_the_documented_four() {
        let mut optional: Vec<(&str, &str, &str)> = Vec::new();
        for entity in Entity::ALL {
            for edge in entity.base_edges() {
                if !edge.meta.mandatory {
                    optional.push((entity.as_str(), edge.from, edge.to));
                }
            }
        }
        optional.sort_unstable();
        assert_eq!(
            optional,
            vec![
                ("PURCHASE_ORDER", "SUBMITTED", "CLOSED"),
                ("STOCK_IN", "DRAFT", "POSTED"),
                ("STOCK_IN", "PENDING_POST", "CANCELLED"),
                ("STOCK_OUT", "DRAFT", "POSTED"),
            ],
            "可选边（可被配置禁用）必须与文档一致：多一条 = 偷偷放宽了配置面"
        );
    }

    // ==================== 【D1】第 2/3 层：合并规则（纯函数） ====================

    fn d1_row(entity: &str, from: &str, to: &str, enabled: bool) -> EdgeConfigRow {
        EdgeConfigRow {
            id: Uuid::now_v7(),
            entity: entity.to_string(),
            from_status: from.to_string(),
            to_status: to.to_string(),
            enabled,
            requires_reason: false,
            label: None,
            auth_code: None,
        }
    }

    /// ❸ 禁用一条 optional 边 → 该边从有效边里消失，且不报 WARN（这是合法配置）。
    #[test]
    fn disabled_optional_edge_disappears() {
        let rows = [d1_row(ENTITY_STOCK_IN, "DRAFT", "POSTED", false)];
        let resolution = resolve_edges(Entity::StockIn, &rows);
        assert!(!resolution.allows("DRAFT", "POSTED"), "禁用的可选边不能出现在有效边里");
        assert!(resolution.allows("DRAFT", "PENDING_POST"), "同状态的其他边不受影响");
        assert_eq!(resolution.edges.len(), STOCK_IN_BASE.len() - 1, "只少了这一条");
        assert!(
            resolution.warnings.is_empty(),
            "合法禁用不该有 WARN：{:?}",
            resolution.warnings
        );
    }

    /// ❹ 禁用 mandatory 边 → **无效**（边仍可用）+ WARN。
    #[test]
    fn disabling_mandatory_edge_is_ignored_with_warning() {
        let rows = [d1_row(ENTITY_STOCK_IN, "POSTED", "CANCELLED", false)];
        let resolution = resolve_edges(Entity::StockIn, &rows);
        assert!(
            resolution.allows("POSTED", "CANCELLED"),
            "硬约束边（红冲）不许被配置删掉"
        );
        assert!(
            resolution.warnings.iter().any(|warning| warning.contains("mandatory")),
            "必须留下 WARN 说明配置被忽略：{:?}",
            resolution.warnings
        );
    }

    /// ❺ 配置添加 BASE 里没有的边 → 被忽略 + WARN（不许「任意跳任意」）。
    #[test]
    fn invented_edge_is_ignored_with_warning() {
        let rows = [
            d1_row(ENTITY_STOCK_IN, "DRAFT", "PENDING_CANCEL", true),
            d1_row(ENTITY_STOCK_IN, "CANCELLED", "POSTED", true),
        ];
        let resolution = resolve_edges(Entity::StockIn, &rows);
        assert!(!resolution.allows("DRAFT", "PENDING_CANCEL"));
        assert!(!resolution.allows("CANCELLED", "POSTED"), "不能改方向 / 不能从终态复活");
        assert_eq!(
            resolution.warnings.len(),
            2,
            "两条脏边各一条 WARN：{:?}",
            resolution.warnings
        );
        assert!(
            resolution
                .warnings
                .iter()
                .all(|warning| warning.contains("不在 BASE_EDGES"))
        );
    }

    /// ❻ 配置数据脏（entity 写错）→ 整组忽略 + WARN，该实体退回 BASE_EDGES。
    #[test]
    fn dirty_entity_is_ignored_with_warning() {
        let rows = [d1_row("purchase_order", "SUBMITTED", "CLOSED", false)];
        let resolution = resolve_edges(Entity::PurchaseOrder, &rows);
        assert!(
            resolution.allows("SUBMITTED", "CLOSED"),
            "entity 写错的行不该生效（否则一半配置生效一半不生效，谁也不敢动）"
        );
        assert!(
            resolution
                .warnings
                .iter()
                .any(|warning| warning.contains("不在状态机注册表里")),
            "{:?}",
            resolution.warnings
        );
    }

    /// ❼ label / requires_reason / auth_code 三项加严会被合并进有效边。
    #[test]
    fn label_reason_and_auth_code_are_merged() {
        let mut row = d1_row(ENTITY_STOCK_IN, "DRAFT", "PENDING_POST", true);
        row.label = Some("报批".to_string());
        row.requires_reason = true;
        row.auth_code = Some("erp:stock-in:submit".to_string());
        let resolution = resolve_edges(Entity::StockIn, &[row]);
        let edge = resolution.get("DRAFT", "PENDING_POST").expect("边应存在");
        assert_eq!(edge.label, "报批", "显示文案要能改");
        assert!(edge.requires_reason, "配置可以加「必填原因」");
        assert_eq!(edge.auth_code.as_deref(), Some("erp:stock-in:submit"));
        // 其它边不受影响
        assert_eq!(resolution.get("DRAFT", "CANCELLED").unwrap().label, "草稿作废");
    }

    /// ❽ 只许加严：配置把「代码要求原因」的边写成 `requires_reason = false` 也照样要求。
    #[test]
    fn requires_reason_cannot_be_weakened() {
        let mut row = d1_row(ENTITY_STOCK_IN, "POSTED", "CANCELLED", true);
        row.requires_reason = false;
        let resolution = resolve_edges(Entity::StockIn, &[row]);
        assert!(
            resolution.get("POSTED", "CANCELLED").unwrap().requires_reason,
            "红冲必须给原因，配置关不掉"
        );
    }

    // ==================== 【D1】配置写入：权限 / 边界 / 留痕（真库） ====================

    /// 超管声明（改配置必须有）。
    fn d1_super_claims() -> TokenClaims {
        d1_claims(&["*:*:*"], &["SUPER_ADMIN"])
    }

    /// 普通用户声明。
    fn d1_claims(permissions: &[&str], roles: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "00000000-0000-4000-8000-0000000000a1".to_string(),
            username: Some("d1_tester".to_string()),
            roles: roles.iter().map(|value| value.to_string()).collect(),
            permissions: permissions.iter().map(|value| value.to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: HashMap::new(),
        }
    }

    /// 起一个**可回滚**的事务：配置与流转都写在里面，测试结束 rollback → 库里零残留。
    ///
    /// 为什么不用 `setup()` 的前缀清理：配置行按真实 entity 存（`STOCK_IN` 之类），
    /// 没有可用的夹具前缀；事务回滚是唯一不会误删别人配置的做法。
    async fn d1_tx() -> Option<(tokio::sync::MutexGuard<'static, ()>, Transaction)> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        match db().begin().await {
            Ok(tx) => Some((guard, tx)),
            Err(error) => {
                println!("SKIP: 开事务失败（{error}）");
                None
            }
        }
    }

    fn d1_patch(entity: &str, from: &str, to: &str) -> EdgeConfigPatch {
        EdgeConfigPatch::allow(entity, from, to)
    }

    async fn d1_audit_count(tx: &Transaction, entity_no_like: &str) -> i64 {
        tx.scalar_sql::<i64>(
            "SELECT count(*) FROM biz_status_log \
              WHERE entity_type = ? AND entity_no LIKE ?"
                .to_string(),
            vec![
                Value::Text(ENTITY_EDGE_CONFIG.to_string()),
                Value::Text(entity_no_like.to_string()),
            ],
        )
        .await
        .unwrap_or(0)
    }

    /// ❾ 改配置的门：非超管 403、没二次确认 400、BASE 里没有的边 400、
    /// 禁用 mandatory 边 400、把必填原因放宽 400。**一条都不许过**。
    #[tokio::test]
    async fn config_write_rejects_every_forbidden_action() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };

        // ① 非超管
        let err = upsert_edge_config(
            &tx,
            &d1_claims(&["erp:stock-in:update"], &["USER"]),
            &d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 403, "{}", err.msg);
        assert!(err.msg.contains("SUPER_ADMIN"), "{}", err.msg);

        // ② 没二次确认
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED");
        patch.confirm = false;
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("二次确认"), "{}", err.msg);

        // ③ BASE 里没有的边（不能创造新边）
        let err = upsert_edge_config(
            &tx,
            &d1_super_claims(),
            &d1_patch(ENTITY_STOCK_IN, "DRAFT", "PENDING_CANCEL"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("不在 BASE_EDGES"), "{}", err.msg);

        // ④ 禁用 mandatory 边
        let mut patch = d1_patch(ENTITY_STOCK_IN, "POSTED", "CANCELLED");
        patch.enabled = false;
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("硬约束边"), "{}", err.msg);

        // ⑤ 把「代码要求原因」的边放宽
        let patch = d1_patch(ENTITY_STOCK_IN, "POSTED", "CANCELLED");
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("放宽"), "{}", err.msg);

        let _ = tx.rollback().await;
    }

    /// ❿ 写配置成功 → 落一行 + `biz_status_log` 有改前/改后快照（留痕）。
    #[tokio::test]
    async fn config_write_persists_and_leaves_audit() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let row = upsert_edge_config(&tx, &d1_super_claims(), &d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED"))
            .await
            .expect("超管 + confirm 应写入成功");
        assert!(row.enabled);

        let listed = list_edge_config(&tx, Some(Entity::StockIn)).await.expect("读配置");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].from_status, "DRAFT");
        assert_eq!(listed[0].to_status, "POSTED");

        let audits = d1_audit_count(&tx, "STOCK_IN DRAFT>POSTED").await;
        assert_eq!(audits, 1, "改配置必须留痕（biz_status_log）");
        let reason: String = tx
            .scalar_sql::<String>(
                "SELECT reason FROM biz_status_log \
                  WHERE entity_type = ? AND entity_no = ? ORDER BY created_at DESC LIMIT 1"
                    .to_string(),
                vec![
                    Value::Text(ENTITY_EDGE_CONFIG.to_string()),
                    Value::Text("STOCK_IN DRAFT>POSTED".to_string()),
                ],
            )
            .await
            .expect("读审计");
        assert!(reason.contains("改前"), "审计要写改前：{reason}");
        assert!(reason.contains("改后"), "审计要写改后：{reason}");
        assert!(reason.contains("enabled=true"), "{reason}");
        // `--nocapture` 时把审计原文打出来（交付证据里直接贴这一行）
        println!("[D1 证据] biz_status_log(entity_type=FLOW_EDGE_CFG) reason = {reason}");
        println!(
            "[D1 证据] biz_flow_state_edge 行 = entity:{} {}>{} enabled={} requires_reason={} label={:?} auth_code={:?}",
            listed[0].entity,
            listed[0].from_status,
            listed[0].to_status,
            listed[0].enabled,
            listed[0].requires_reason,
            listed[0].label,
            listed[0].auth_code
        );

        let _ = tx.rollback().await;
    }

    /// ⓫ 禁用 optional 边后，**真走这条边** → 409（业务语言，不暴露内部单号）；删配置后恢复。
    ///
    /// 【E2E-FIX-1】同时钉住三件事：
    /// - 禁用主链路边必须先过 `confirm_critical`（不传 → 400 并点名业务后果）；
    /// - 用户侧 `msg` 只有业务语言（内部单号 / 状态名只在 `internal_detail`，只进日志）；
    /// - 删掉配置后这条边必须恢复可用（主链不能永久断）。
    #[tokio::test]
    async fn disabled_edge_is_rejected_then_restored_after_delete() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let no = format!("{PREFIX}D1A");
        // ① 不传 confirm_critical → 400（主链路边禁用要第三次确认）
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED");
        patch.enabled = false;
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("主链路必需边"), "{}", err.msg);
        assert!(
            err.msg.contains("采购收货无法直接过账"),
            "报文要点名业务后果：{}",
            err.msg
        );
        assert!(
            err.msg.contains("confirm_critical=true"),
            "报文要告诉调用方怎么继续：{}",
            err.msg
        );

        // ② 传了 confirm_critical 才允许禁用
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED").with_confirm_critical(true);
        patch.enabled = false;
        upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .expect("确认后禁用可选边应成功");

        // ③ 真走这条边 → 409，报文是业务语言
        let err = transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("采购收货无法直接过账"),
            "文案要说清业务后果：{}",
            err.msg
        );
        assert!(
            err.msg.contains("已被系统配置禁用") && err.msg.contains("请联系系统管理员"),
            "文案要给出下一步（找管理员）：{}",
            err.msg
        );
        assert!(!err.msg.contains(&no), "用户侧报文不许暴露内部单号：{}", err.msg);
        assert!(
            !err.msg.contains("DRAFT") && !err.msg.contains("POSTED"),
            "用户侧报文不许出现状态机状态名：{}",
            err.msg
        );
        let detail = err
            .internal_detail
            .as_deref()
            .expect("内部单号 / 流转 / 可选去向必须留在 internal_detail（只进日志）");
        assert!(detail.contains(&no), "internal_detail 要带单号：{detail}");
        assert!(
            detail.contains("DRAFT") && detail.contains("POSTED"),
            "internal_detail 要带状态流转：{detail}"
        );

        // ④ 删配置 → 回到 BASE 行为
        assert!(
            delete_edge_config(&tx, &d1_super_claims(), ENTITY_STOCK_IN, "DRAFT", "POSTED")
                .await
                .expect("删配置")
        );
        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .expect("删掉配置后这条边必须恢复可用");
        let _ = tx.rollback().await;
    }

    /// 【E2E-FIX-1】`biz_flow_critical_edge`（V0354 种进库的巡检白名单）与代码里的
    /// [`CRITICAL_EDGES`] **逐行一致**：漏一条 → 巡检看不见 → 主链被关也不报警；
    /// 多一条 → 巡检对一条不存在的边报警。两边漂移一律红。
    #[tokio::test]
    async fn critical_edges_match_db_table() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        // 表不存在 = V0354 没跑（老库）：明确 SKIP 而不是假装通过
        let exists = tx
            .scalar_sql::<i64>(
                "SELECT count(*) FROM information_schema.tables \
                  WHERE table_schema = 'public' AND table_name = 'biz_flow_critical_edge'"
                    .to_string(),
                vec![],
            )
            .await
            .unwrap_or(0);
        if exists == 0 {
            println!("SKIP: biz_flow_critical_edge 不存在（该库还没跑 V0354）");
            let _ = tx.rollback().await;
            return;
        }
        let rows = tx
            .query_sql(
                "SELECT entity, from_status, to_status, impact FROM biz_flow_critical_edge".to_string(),
                vec![],
            )
            .await
            .expect("读 biz_flow_critical_edge");
        let mut db: Vec<(String, String, String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.get_typed::<String>("entity").unwrap_or_default(),
                    row.get_typed::<String>("from_status").unwrap_or_default(),
                    row.get_typed::<String>("to_status").unwrap_or_default(),
                    row.get_typed::<String>("impact").unwrap_or_default(),
                )
            })
            .collect();
        db.sort();
        let mut code: Vec<(String, String, String, String)> = CRITICAL_EDGES
            .iter()
            .map(|edge| {
                (
                    edge.entity.as_str().to_string(),
                    edge.from.to_string(),
                    edge.to.to_string(),
                    edge.impact.to_string(),
                )
            })
            .collect();
        code.sort();
        assert_eq!(
            db, code,
            "biz_flow_critical_edge 与代码里的 CRITICAL_EDGES 不一致：\
             改清单必须同步加一条新迁移（不能改已应用的 V0354）"
        );
        println!(
            "[E2E-FIX-1 证据] biz_flow_critical_edge {} 行，与 CRITICAL_EDGES 逐行一致",
            db.len()
        );
        let _ = tx.rollback().await;
    }

    /// 【E2E-FIX-1】主链路清单本身的自洽性（**不连库**）：
    /// 每一条都必须是 `BASE_EDGES` 里真实存在的边（写错状态名 = 标记永远不生效，
    /// 却会让巡检对一条不存在的边报警，两头都骗）。
    #[test]
    fn critical_edges_are_real_base_edges() {
        for critical in CRITICAL_EDGES {
            let entity = critical.entity;
            assert!(
                entity.base_edge(critical.from, critical.to).is_some(),
                "{} {}→{} 不在 BASE_EDGES 里（状态名写错了？）",
                entity.as_str(),
                critical.from,
                critical.to
            );
            assert!(
                !critical.impact.trim().is_empty(),
                "{} {}→{} 缺 impact：错误文案与巡检报警都要用它",
                entity.as_str(),
                critical.from,
                critical.to
            );
            // impact 会拼进「让{impact}」「{impact}：」两种句式，句尾带句号会读成断句
            assert!(
                !critical.impact.ends_with('。') && !critical.impact.ends_with('，'),
                "impact 不要带句尾标点（会被拼进句子中间）：{}",
                critical.impact
            );
        }
        // E2E-02 实际踩到的那一条必须在清单里（回归钉死）
        assert!(
            critical_edge(Entity::StockOut, "DRAFT", "POSTED").is_some(),
            "STOCK_OUT DRAFT→POSTED 被关掉会让整条销售链断在发货，必须标记"
        );
        // 卡面点名的主链边一条不落
        for (entity, from, to) in [
            (Entity::StockOut, "DRAFT", "PENDING_POST"),
            (Entity::StockIn, "DRAFT", "PENDING_POST"),
            (Entity::StockIn, "PENDING_POST", "POSTED"),
            (Entity::PurchaseOrder, "DRAFT", "SUBMITTED"),
            (Entity::PurchaseOrder, "SUBMITTED", "APPROVED"),
            (Entity::SaleOrder, "DRAFT", "SUBMITTED"),
            (Entity::SaleOrder, "SUBMITTED", "APPROVED"),
            (Entity::Transfer, "DRAFT", "PENDING"),
            (Entity::Transfer, "IN_TRANSIT", "DONE"),
            (Entity::StockTake, "DRAFT", "PENDING_POST"),
        ] {
            assert!(
                critical_edge(entity, from, to).is_some(),
                "{} {from}→{to} 是主链路边，必须在 CRITICAL_EDGES 里",
                entity.as_str()
            );
        }
    }

    /// 「一条边只能标一次」+ 查询口径（`None` = 不是主链路边）。
    #[test]
    fn critical_edges_are_unique_and_lookup_is_exact() {
        let mut seen = std::collections::HashSet::new();
        for critical in CRITICAL_EDGES {
            assert!(
                seen.insert((critical.entity.as_str(), critical.from, critical.to, critical.impact)),
                "CRITICAL_EDGES 有重复行：{} {}→{}（impact 也要一致）",
                critical.entity.as_str(),
                critical.from,
                critical.to
            );
        }
        assert!(
            critical_edge(Entity::StockOut, "POSTED", "CANCELLED").is_none(),
            "作废边不是主链功能边，不该被标 critical"
        );
        assert!(
            critical_edge(Entity::StockOut, "DRAFT", "CANCELLED").is_none(),
            "草稿作废不是主链功能边"
        );
    }

    /// 禁用主链路边必须二次确认这件事，**纯函数层面**也要钉住（不靠真库）：
    /// `confirm_critical` 是独立字段，不能被 `confirm` / `allow()` 顺手置真。
    #[test]
    fn confirm_critical_is_independent_of_confirm() {
        let patch = EdgeConfigPatch::allow("STOCK_OUT", "DRAFT", "POSTED");
        assert!(patch.confirm, "allow() 的 confirm 恒 true");
        assert!(
            !patch.confirm_critical,
            "allow() 不能顺手把「禁用主链路边」的确认置真 —— 那等于没有确认"
        );
        assert!(patch.with_confirm_critical(true).confirm_critical);
    }

    /// ⓬ 有人绕过 API 直接写库禁用 mandatory 边 → 运行时忽略 + WARN，边照旧可用。
    #[tokio::test]
    async fn raw_sql_mandatory_disable_is_ignored_at_runtime() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        tx.exec_sql(
            "INSERT INTO biz_flow_state_edge (entity, from_status, to_status, enabled) \
             VALUES (?, 'POSTED', 'CANCELLED', false)"
                .to_string(),
            vec![Value::Text(ENTITY_STOCK_IN.to_string())],
        )
        .await
        .expect("绕过 API 直接写脏配置");

        let resolution = edge_resolution(&tx, Entity::StockIn).await;
        assert!(
            resolution.allows("POSTED", "CANCELLED"),
            "脏配置禁用 mandatory 边必须被忽略"
        );
        assert!(
            resolution.warnings.iter().any(|warning| warning.contains("mandatory")),
            "{:?}",
            resolution.warnings
        );
        // 真流转也照旧能走
        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}D1B"),
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            Some("客户退货"),
            None,
        )
        .await
        .expect("红冲不该被脏配置挡住");
        let _ = tx.rollback().await;
    }

    /// ⓭ 配置加 `requires_reason` → 走这条边必须给原因（400），给了就放行。
    #[tokio::test]
    async fn config_requires_reason_on_edge() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED");
        patch.requires_reason = true;
        upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .expect("加必填原因应成功");

        let no = format!("{PREFIX}D1C");
        let err = transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("reason"), "{}", err.msg);

        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            Some("补录历史入库"),
            None,
        )
        .await
        .expect("给了原因应放行");
        let _ = tx.rollback().await;
    }

    /// ⓮ 配置加 `auth_code` → 无权限的人走该边 403；有权限的人放行。
    #[tokio::test]
    async fn config_auth_code_gates_operator() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED");
        patch.auth_code = Some("erp:stock-in:post".to_string());
        upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .expect("加权限码应成功");

        let no = format!("{PREFIX}D1D");
        let err = transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            &d1_claims(&["erp:stock-in:update"], &["USER"]),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 403, "{}", err.msg);
        assert!(err.msg.contains("erp:stock-in:post"), "{}", err.msg);

        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            &d1_claims(&["erp:stock-in:post"], &["USER"]),
            None,
            None,
        )
        .await
        .expect("持有权限码应放行");
        let _ = tx.rollback().await;
    }

    /// ⓯ DB 表不存在（迁移没跑 / 表被误删）→ 退化 BASE_EDGES + WARN，系统不崩。
    ///
    /// 做法：事务里把表改名（DDL 在 PG 里也是事务性的），读一次 + **真跑一次流转**，再回滚。
    ///
    /// 真跑流转这一句是必须的：真机验收实测，读配置失败会把**整个事务置为 aborted**
    /// （PG 语义），紧接着的 `insert_log` 直接 `query failed` —— 降级读把业务写死了。
    /// 修复是 `load_edge_config_rows` 里套 `SAVEPOINT`；这条用例就是那次事故的回归。
    #[tokio::test]
    async fn missing_table_degrades_to_base_edges() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        tx.exec_sql(
            "ALTER TABLE biz_flow_state_edge RENAME TO biz_flow_state_edge_d1_gone".to_string(),
            vec![],
        )
        .await
        .expect("事务内改名（这个 DDL 可回滚）");

        let resolution = edge_resolution(&tx, Entity::StockIn).await;
        let rendered = format!("{:?}", resolution.warnings);
        let no = format!("{PREFIX}D1GONE");
        let written = transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await;
        let trail = trail_of_no(&tx, &no).await.map(|rows| rows.len()).unwrap_or(0);
        let _ = tx.rollback().await;

        assert_eq!(
            resolution.edges.len(),
            STOCK_IN_BASE.len(),
            "表不存在时必须完整退回 BASE_EDGES"
        );
        assert!(resolution.allows("DRAFT", "POSTED"));
        assert!(
            rendered.contains("读取 biz_flow_state_edge 失败"),
            "必须留 WARN 说明退化了：{rendered}"
        );
        assert!(
            written.is_ok(),
            "表不存在时流转也必须成功（fail-safe）：{:?}",
            written.err().map(|error| error.msg)
        );
        assert_eq!(trail, 1, "降级路径也要留下状态日志（事务没被 aborted）");
    }

    /// ⓰ 配置改 label → 显示文案变了（真库走一遍 upsert + 合并）。
    #[tokio::test]
    async fn config_label_changes_display_text() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "PENDING_POST");
        patch.label = Some("报批".to_string());
        upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .expect("改文案应成功");

        let resolution = edge_resolution(&tx, Entity::StockIn).await;
        assert_eq!(resolution.get("DRAFT", "PENDING_POST").unwrap().label, "报批");
        let _ = tx.rollback().await;
    }

    /// ⓱ 重复删除幂等：没有配置时删除返回 `false`，且不留审计噪音。
    #[tokio::test]
    async fn delete_without_config_is_idempotent() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        assert!(
            !delete_edge_config(&tx, &d1_super_claims(), ENTITY_STOCK_IN, "DRAFT", "POSTED")
                .await
                .expect("删一个不存在的配置不该报错")
        );
        assert_eq!(d1_audit_count(&tx, "STOCK_IN DRAFT>POSTED").await, 0);
        // 非合法边直接 400（连「删」都不允许出现凭空边）
        let err = delete_edge_config(&tx, &d1_super_claims(), ENTITY_STOCK_IN, "DRAFT", "DONE")
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        let _ = tx.rollback().await;
    }

    /// ⒅ 操作人只有 `id` 时，`operator_name` 必须按 `sys_user` 快照补齐（FIX-9 ③）。
    ///
    /// 生产上绝大多数状态变更走的是事务体 / model 路径，手上只有 `Uuid`
    /// （`Operator::of(Some(id))`），以前这类日志的 `operator_name` 一律为空 ——
    /// 用户改名 / 销号之后审计只剩一串 uuid。修法是在 `insert_log` 里补一次主键查询：
    /// **优先 `real_name`，没填退回 `username`**。
    #[tokio::test]
    async fn operator_name_snapshot_filled_when_only_id_given() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let Some(row) = tx
            .query_first_sql(
                "SELECT id, COALESCE(NULLIF(btrim(real_name), ''), username) AS name \
                   FROM sys_user WHERE username = 'admin'"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查 admin")
        else {
            println!("SKIP: 库里没有 admin 用户");
            return;
        };
        let user_id: Uuid = row.get_typed("id").expect("admin id");
        let expected: String = row.get_typed("name").expect("admin 姓名快照");
        let no = format!("{PREFIX}NAME");
        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            Operator::of(Some(user_id)),
            None,
            None,
        )
        .await
        .expect("流转应成功（姓名快照是旁路，不许阻断业务）");
        let trail = trail_of_no(&tx, &no).await.expect("读状态日志");
        let _ = tx.rollback().await;

        assert_eq!(trail.len(), 1, "应写一行状态日志");
        assert_eq!(trail[0].operator_id, Some(user_id), "operator_id 保持原值");
        assert_eq!(
            trail[0].operator_name.as_deref(),
            Some(expected.as_str()),
            "只有 id 的操作人也必须写出姓名快照（real_name 优先，退回 username）"
        );
    }

    // ==================== 人力资源（HR-08 · ARCH-9 §4.2）真库口径 ====================
    //
    // 文件末尾 `hr_state_machine_tests` 那 11 条是**纯注册表**用例（不连库、永远真跑）；
    // 这三条补的是「注册表对了，真写库也对不对」：HR 五个实体的 `entity_type` 是本次
    // 新登记的字符串，`as_str()` 一旦与 `DocKind` / 表名漂了、或哪天有人给
    // `biz_status_log.entity_type` 加白名单 CHECK，只有真库能发现。
    //
    // 每条用例都用一个只属于本卡的 `entity_no` 前缀（`P10SM-`），`cleanup()` 会一并清掉。

    /// HR 五实体的合法边逐条真跑：`biz_status_log` 的 `entity_type` 落的必须是
    /// `DocKind` 值（`EMPLOYEE` / `EMPLOYEE_CONTRACT` / `LEAVE_REQUEST` /
    /// `ATTENDANCE_PERIOD` / `SALARY_SHEET`），且必填原因的边走真库时也拦住并落 reason。
    #[tokio::test]
    async fn hr_legal_edges_write_status_log_for_every_entity() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // (实体, from, to) —— 每个实体一条「主链」合法边，含两条必填原因边。
        let cases: [(Entity, &str, &str); 5] = [
            (Entity::Employee, "PROBATION", "ACTIVE"),
            (Entity::EmployeeContract, "ACTIVE", "TERMINATED"),
            (Entity::LeaveRequest, "SUBMITTED", "APPROVED"),
            (Entity::AttendancePeriod, "LOCKED", "DRAFT"),
            (Entity::SalarySheet, "SUBMITTED", "CALCULATED"),
        ];
        for (entity, from, to) in cases {
            let no = format!("{PREFIX}HR-{}", entity.as_str());
            let needs = entity.needs_reason(from, to);
            let outcome = transition(
                db(),
                entity.as_str(),
                Uuid::now_v7(),
                &no,
                entity.edges(),
                from,
                to,
                op(),
                if needs {
                    Some("真库用例：人力资源状态变更原因")
                } else {
                    None
                },
                None,
            )
            .await
            .unwrap_or_else(|err| panic!("{:?} 的 {from} → {to} 真库流转失败：{}", entity, err.msg));
            assert_eq!(outcome.from, from);
            assert_eq!(outcome.to, to);

            let trail = trail_of_no(db(), &no).await.unwrap();
            assert_eq!(trail.len(), 1, "{:?} 应写一行状态日志", entity);
            let row = &trail[0];
            // 落库值 = DocKind 值（巡检与溯源都按它筛实体）
            assert_eq!(row.entity_type, entity.as_str());
            assert!(
                DocKind::parse(&row.entity_type).is_some(),
                "{:?} 的 entity_type「{}」不在 DocKind 域内，巡检会把它当路灯",
                entity,
                row.entity_type
            );
            assert_eq!(row.from_status.as_deref(), Some(from));
            assert_eq!(row.to_status, to);
            assert_eq!(row.operator_name.as_deref(), Some("p10_tester"));
            assert_eq!(row.id, outcome.log_id, "{:?} 的 log_id 必须是真落库那行", entity);
            if needs {
                assert_eq!(
                    row.reason.as_deref(),
                    Some("真库用例：人力资源状态变更原因"),
                    "{:?} 的必填原因没落库（审计查不到「为什么」）",
                    entity
                );
            }
        }
    }

    /// HR 五实体的非法边：真库口径必须 409，且**一行日志都不许写**。
    ///
    /// 每条都挑「最容易被误加」的那条：离职复活、到期合同被解除、审批中作废、
    /// 考勤跳级锁定、未审批直接过账。
    #[tokio::test]
    async fn hr_illegal_edges_are_conflict_and_write_no_log() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let cases: [(Entity, &str, &str, &str, bool); 5] = [
            // (实体, 当前态, 被拒的目标态, 文案里应点名的词, 是否点名「被拒的目标态」)
            // 终态的员工没有出边，`rejection_message` 只解释「已是终态」、不点目标态。
            (Entity::Employee, "RESIGNED", "ACTIVE", "终态", false),
            (Entity::EmployeeContract, "EXPIRED", "TERMINATED", "RENEWED", true),
            (Entity::LeaveRequest, "SUBMITTED", "CANCELLED", "APPROVED", true),
            (Entity::AttendancePeriod, "DRAFT", "LOCKED", "SUBMITTED", true),
            (Entity::SalarySheet, "CALCULATED", "POSTED", "SUBMITTED", true),
        ];
        for (entity, from, to, named, names_target) in cases {
            let no = format!("{PREFIX}HR-BAD-{}", entity.as_str());
            let err = transition(
                db(),
                entity.as_str(),
                Uuid::now_v7(),
                &no,
                entity.edges(),
                from,
                to,
                op(),
                Some("非法边不该走到 reason 校验"),
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(err.status, 409, "{:?} {from} → {to}：{}", entity, err.msg);
            assert!(err.msg.contains(from), "{:?} 文案要点名当前态：{}", entity, err.msg);
            if names_target {
                assert!(err.msg.contains(to), "{:?} 文案要点名被拒的目标态：{}", entity, err.msg);
            }
            assert!(
                err.msg.contains(named),
                "{:?} 文案要点名允许的去向「{named}」：{}",
                entity,
                err.msg
            );
            assert_eq!(
                count_of(&no).await,
                0,
                "{:?} 的非法边写了状态日志 —— 单据没变、日志说有，这是脏轨迹",
                entity
            );
        }
    }

    /// HR 必填原因的边：缺 reason 真库口径是 400（不是 500、不是放行），补上才落库。
    #[tokio::test]
    async fn hr_reason_edges_require_reason_on_real_db() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let cases: [(Entity, &str, &str); 4] = [
            (Entity::Employee, "ACTIVE", "SUSPENDED"),
            (Entity::EmployeeContract, "ACTIVE", "TERMINATED"),
            (Entity::AttendancePeriod, "LOCKED", "DRAFT"),
            (Entity::SalarySheet, "POSTED", "REVERSED"),
        ];
        for (entity, from, to) in cases {
            let no = format!("{PREFIX}HR-RS-{}", entity.as_str());
            let err = transition(
                db(),
                entity.as_str(),
                Uuid::now_v7(),
                &no,
                entity.edges(),
                from,
                to,
                op(),
                None,
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(err.status, 400, "{:?} {from} → {to} 缺 reason：{}", entity, err.msg);
            assert!(err.msg.contains("reason"), "{:?} 文案要提原因：{}", entity, err.msg);
            assert_eq!(count_of(&no).await, 0, "{:?} 400 之后不许留日志", entity);

            // 补上原因就放行（同一条边、同一个单号）
            transition(
                db(),
                entity.as_str(),
                Uuid::now_v7(),
                &no,
                entity.edges(),
                from,
                to,
                op(),
                Some("真库用例：必填原因已给"),
                None,
            )
            .await
            .unwrap_or_else(|err| panic!("{:?} 给了 reason 仍失败：{}", entity, err.msg));
            assert_eq!(count_of(&no).await, 1, "{:?} 给了 reason 应落一行", entity);
        }
    }
}

// ==================== 合同状态机（A1 · ARCH-2 §4） ====================
//
// 这一组**不连库**（纯注册表 / 边表断言），因此不受 `ERP_DB_URL` 开关影响 ——
// 合同状态机的全部合法边与非法边都必须**每次 cargo test 都真跑**。
// 上一组 `mod tests` 里的 `transition()` 用例才是真库用例（写 `biz_status_log`）。
#[cfg(test)]
mod contract_tests {
    use super::*;

    /// 合同的 5 个状态（ARCH-2 §2.2 的 `ck_biz_contract_status` 白名单，逐字一致）。
    const CONTRACT_STATUSES: [&str; 6] = [
        "DRAFT",
        "PENDING_APPROVAL",
        "SIGNED",
        "ACTIVE",
        "COMPLETED",
        "TERMINATED",
    ];

    /// ARCH-2 §4.1 + ARCH-3 R-P1-1 的 7 条合法边（照抄文档表格，用来与注册表对拍）。
    const EXPECTED_EDGES: [(&str, &str); 7] = [
        ("DRAFT", "PENDING_APPROVAL"),
        ("PENDING_APPROVAL", "SIGNED"),
        ("PENDING_APPROVAL", "DRAFT"),
        ("SIGNED", "ACTIVE"),
        ("SIGNED", "TERMINATED"),
        ("ACTIVE", "COMPLETED"),
        ("ACTIVE", "TERMINATED"),
    ];

    /// 禁止边（逐条列举）。R-P1-1 之后 **`DRAFT → SIGNED` / `DRAFT → ACTIVE` 都成了禁止边**
    /// —— 这正是「没人审就生效」在状态机层面的封堵点，必须一条不落地钉住。
    const FORBIDDEN_EDGES: [(&str, &str); 23] = [
        ("DRAFT", "SIGNED"),
        ("DRAFT", "ACTIVE"),
        ("DRAFT", "COMPLETED"),
        ("DRAFT", "TERMINATED"),
        ("PENDING_APPROVAL", "ACTIVE"),
        ("PENDING_APPROVAL", "COMPLETED"),
        ("PENDING_APPROVAL", "TERMINATED"),
        ("SIGNED", "DRAFT"),
        ("SIGNED", "PENDING_APPROVAL"),
        ("SIGNED", "COMPLETED"),
        ("ACTIVE", "DRAFT"),
        ("ACTIVE", "PENDING_APPROVAL"),
        ("ACTIVE", "SIGNED"),
        ("COMPLETED", "DRAFT"),
        ("COMPLETED", "PENDING_APPROVAL"),
        ("COMPLETED", "SIGNED"),
        ("COMPLETED", "ACTIVE"),
        ("COMPLETED", "TERMINATED"),
        ("TERMINATED", "DRAFT"),
        ("TERMINATED", "SIGNED"),
        ("TERMINATED", "ACTIVE"),
        ("TERMINATED", "COMPLETED"),
        ("TERMINATED", "PENDING_APPROVAL"),
    ];

    /// ⑦ 七处注册全部生效：变体 / ALL / as_str / label / edges / reason_edges / locked_statuses。
    #[test]
    fn contract_registered_in_all_seven_places() {
        // ① enum 变体存在 + ② ALL 收录
        assert!(
            Entity::ALL.contains(&Entity::Contract),
            "Entity::ALL 必须包含 Contract（漏登记 = transition 直接 500）"
        );
        assert_eq!(
            Entity::ALL.len(),
            29,
            "注册表从 7 → 8（CONTRACT）→ 9（STOCK_OUT）→ 10（STOCK_TAKE）→ 11（BUDGET，FIN-1）\
             → 12（FIXED_ASSET，FIN-2）→ 13（DEPRECIATION，FIN-2）→ 14（EXPENSE_REPORT，FIN-3）\
             → 15（SALE_ORDER，销售链 SO-05）→ 16（SHIPMENT，销售链 SO-09）\
             → 17（RECEIVABLE，销售链 SO-13）→ 18（RECEIPT，销售链 SO-17）\
             → 19（RECEIPT_REFUND，销售链 SO-14 遗留 D10）→ 20（INVOICE，销售链 SO-20）\
             → 21（SALES_RETURN，销售链 SO-29）→ 22（PURCHASE_RETURN，采购链 P2-03）\
             → 23（COST_OBJECT，成本核算 ARCH-9 §4.1）\
             → 24（COST_PERIOD，成本核算 ARCH-9 §4.1 结算批次）\
             → 29（HR-08：EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / \
             ATTENDANCE_PERIOD / SALARY_SHEET，人力资源 ARCH-9 §4.2）"
        );

        // ③ as_str
        assert_eq!(Entity::Contract.as_str(), "CONTRACT");
        assert_eq!(ENTITY_CONTRACT, "CONTRACT");

        // ④ label
        assert_eq!(Entity::Contract.label(), "合同");

        // ⑥ 字符串 → 实体（未知必须 None，不许静默兜底）
        assert_eq!(Entity::parse("CONTRACT"), Some(Entity::Contract));
        assert_eq!(Entity::parse("CONTRACT "), None, "带空格不匹配，必须报错");
        assert_eq!(Entity::parse("contract"), None, "大小写敏感，落库值是大写");

        // ⑤⑥⑦ 三张表都不空且与常量同源
        assert_eq!(Entity::Contract.edges(), &CONTRACT_EDGES[..]);
        assert_eq!(Entity::Contract.reason_edges(), &CONTRACT_REASON_EDGES[..]);
        assert_eq!(Entity::Contract.locked_statuses(), &CONTRACT_LOCKED[..]);

        // 注册表不变式：as_str 全局唯一（撞串会让 parse 静默认错实体）
        let mut seen = std::collections::BTreeSet::new();
        for entity in Entity::ALL {
            assert!(seen.insert(entity.as_str()), "实体类型串重复：{}", entity.as_str());
        }
        let mut labels = std::collections::BTreeSet::new();
        for entity in Entity::ALL {
            assert!(labels.insert(entity.label()), "中文名重复：{}", entity.label());
        }
    }

    /// ⑤ 每条合法边都被接受，且 `next_statuses` 恰好给出去向。
    #[test]
    fn contract_legal_edges_all_pass() {
        assert_eq!(
            Entity::Contract.edges().len(),
            7,
            "ARCH-3 R-P1-1：A1 的 5 条边 + 「提交审批」与「审批落地/回位」两条 → 7 条"
        );
        for (from, to) in EXPECTED_EDGES {
            assert!(Entity::Contract.can_transition(from, to), "合法边 {from} → {to} 被拒了");
            assert!(
                Entity::Contract.next_statuses(from).contains(&to),
                "{from} 的可选去向里应有 {to}"
            );
        }
        // 逐条给出旁证：每个 from 的去向集合与文档表格一致
        assert_eq!(
            Entity::Contract.next_statuses("DRAFT"),
            vec!["PENDING_APPROVAL"],
            "草稿只有一个去处：提交审批（不许一步签订/生效）"
        );
        assert_eq!(
            Entity::Contract.next_statuses("PENDING_APPROVAL"),
            vec!["SIGNED", "DRAFT"],
            "审批中：审批通过 → SIGNED；驳回/撤回 → DRAFT"
        );
        assert_eq!(Entity::Contract.next_statuses("SIGNED"), vec!["ACTIVE", "TERMINATED"]);
        assert_eq!(
            Entity::Contract.next_statuses("ACTIVE"),
            vec!["COMPLETED", "TERMINATED"]
        );
    }

    /// R-P1-1 的核心判据：**没有任何边能从 DRAFT 一步到 SIGNED / ACTIVE**。
    ///
    /// 这是「合同没人审就生效」在状态机层面的唯一封堵点 —— handler 不做 `match`，
    /// 全靠这张表。所以单独一条用例把它钉死，别让它被后来的「顺手放宽」吃掉。
    #[test]
    fn contract_draft_cannot_reach_signed_or_active_directly() {
        let direct = Entity::Contract.next_statuses("DRAFT");
        assert_eq!(direct, vec!["PENDING_APPROVAL"]);
        assert!(
            !Entity::Contract.can_transition("DRAFT", "SIGNED"),
            "DRAFT → SIGNED 必须禁止：签订只能由审批通过产生"
        );
        assert!(
            !Entity::Contract.can_transition("DRAFT", "ACTIVE"),
            "DRAFT → ACTIVE 必须禁止：生效必须先签订（而签订必须先审批）"
        );
        // 反向：ACTIVE 的唯一入口是 SIGNED → ACTIVE，SIGNED 的唯一入口是审批落地边
        let into_active: Vec<&str> = Entity::Contract
            .edges()
            .iter()
            .filter(|(_, to)| *to == "ACTIVE")
            .map(|(from, _)| *from)
            .collect();
        assert_eq!(into_active, vec!["SIGNED"], "ACTIVE 只能从 SIGNED 来");
        let into_signed: Vec<&str> = Entity::Contract
            .edges()
            .iter()
            .filter(|(_, to)| *to == "SIGNED")
            .map(|(from, _)| *from)
            .collect();
        assert_eq!(
            into_signed,
            vec!["PENDING_APPROVAL"],
            "SIGNED 只能从审批中（审批通过）来"
        );
    }

    /// ⑥ 每条非法边都被拒（逐条断言 + 全组合扫描）。
    #[test]
    fn contract_forbidden_edges_all_rejected() {
        for (from, to) in FORBIDDEN_EDGES {
            assert!(
                !Entity::Contract.can_transition(from, to),
                "非法边 {from} → {to} 竟然被放行（ARCH-2 §4.2 + ARCH-3 R-P1-1）"
            );
        }
        // 23 条禁止边 + 7 条合法边 + 6 条自环 = 6 状态两两组合 36 种，一个不漏
        let mut rejected = 0;
        for from in CONTRACT_STATUSES {
            for to in CONTRACT_STATUSES {
                if EXPECTED_EDGES.contains(&(from, to)) {
                    continue;
                }
                assert!(
                    !Entity::Contract.can_transition(from, to),
                    "{from} → {to} 不在边表里，必须被拒"
                );
                rejected += 1;
            }
        }
        assert_eq!(
            rejected, 29,
            "6 状态两两组合 36 种，去掉 7 条边应剩 29 种被拒（23 条跨态 + 6 条自环）"
        );
    }

    /// 自环（`X → X`，5 条）一律拒绝（§4.2 末段）。
    #[test]
    fn contract_self_loops_all_rejected() {
        for status in CONTRACT_STATUSES {
            assert!(
                !Entity::Contract.can_transition(status, status),
                "自环 {status} → {status} 必须拒绝（transition 只写真实变化）"
            );
        }
    }

    /// 两个终态没有出边；`DRAFT` 是起始态（但 R-P1-1 起它可以被「驳回/撤回」回位）。
    #[test]
    fn contract_terminal_states_have_no_outgoing_edges() {
        for terminal in ["COMPLETED", "TERMINATED"] {
            assert!(
                Entity::Contract.next_statuses(terminal).is_empty(),
                "{terminal} 是终态，不该有出边"
            );
        }
        for from in CONTRACT_STATUSES {
            if from == "COMPLETED" || from == "TERMINATED" {
                assert!(Entity::Contract.next_statuses(from).is_empty());
            }
        }
        let has_incoming = |target: &str| Entity::Contract.edges().iter().any(|(_, to)| *to == target);
        assert!(
            has_incoming("DRAFT"),
            "R-P1-1：DRAFT 有入边（PENDING_APPROVAL → DRAFT 是驳回/撤回回位），不再是纯起始态"
        );
        assert!(has_incoming("PENDING_APPROVAL"), "提交审批边给出了入边");
        assert!(has_incoming("SIGNED"));
        assert!(has_incoming("ACTIVE"));
        assert!(has_incoming("COMPLETED"));
        assert!(has_incoming("TERMINATED"));
        // 起始态（无入边）只有 START 语义的 DRAFT 之外没有别的 —— 这里钉住「自环不存在」
        for (from, to) in Entity::Contract.edges() {
            assert_ne!(from, to, "自环不是状态变更");
        }
    }

    /// ⑥ 必须填原因的两条边 = 两条终止边（§4.1「是否必须给 reason」列）。
    #[test]
    fn contract_reason_edges_are_the_two_terminations() {
        assert_eq!(Entity::Contract.reason_edges().len(), 2);
        assert!(Entity::Contract.needs_reason("SIGNED", "TERMINATED"));
        assert!(Entity::Contract.needs_reason("ACTIVE", "TERMINATED"));
        for (from, to) in [
            ("DRAFT", "PENDING_APPROVAL"),
            ("PENDING_APPROVAL", "SIGNED"),
            ("PENDING_APPROVAL", "DRAFT"),
            ("SIGNED", "ACTIVE"),
            ("ACTIVE", "COMPLETED"),
        ] {
            assert!(!Entity::Contract.needs_reason(from, to), "{from} → {to} 不该强制填原因");
        }
        // reason_edges 必须是 edges 的子集（登记了不存在的边 = 永远触发不了）
        for (from, to) in Entity::Contract.reason_edges() {
            assert!(
                Entity::Contract.can_transition(from, to),
                "reason 边 {from} → {to} 不在合法边表里"
            );
        }
    }

    /// ⑦ locked：`DRAFT` 可自由改，其余（含审批中）一律冻结（§5.3 + R-P1-1）。
    #[test]
    fn contract_locked_statuses_start_after_draft() {
        assert_eq!(Entity::Contract.locked_statuses(), &CONTRACT_LOCKED[..]);
        assert!(!Entity::Contract.is_locked("DRAFT"), "草稿可改");
        for status in ["PENDING_APPROVAL", "SIGNED", "ACTIVE", "COMPLETED", "TERMINATED"] {
            assert!(Entity::Contract.is_locked(status), "{status} 应判为已冻结");
        }
        // locked 只能取状态白名单里的值（写错值等于永不生效）
        for status in Entity::Contract.locked_statuses() {
            assert!(CONTRACT_STATUSES.contains(status), "locked 里的 {status} 不是合同状态");
        }
    }

    /// 非法流转的文案：点名「合同」+ 给出允许去向（前端直接展示）。
    #[test]
    fn contract_rejection_message_names_entity_and_targets() {
        let msg = rejection_message("CONTRACT", "DRAFT", "ACTIVE");
        assert!(msg.contains("合同"), "文案要带单据中文名：{msg}");
        assert!(msg.contains("DRAFT"), "文案要带当前态：{msg}");
        assert!(msg.contains("ACTIVE"), "文案要带被拒的目标态：{msg}");
        assert!(msg.contains("PENDING_APPROVAL"), "文案要给出合法去向：{msg}");
        assert_eq!(
            error_code::ERP_CONTRACT_STATUS_ILLEGAL,
            66015,
            "A2 的 409 用这个码（66013 已被 ERP-B2 占用）"
        );
    }

    /// 卡面与文档的口径分歧**钉死在这里**：合同表没有 `CANCELLED` 态。
    ///
    /// 实现卡正文写「DRAFT → ACTIVE / CANCELLED」，但 ARCH-2 §2.2 的
    /// `ck_biz_contract_status` 白名单是 5 个状态、§4.2 明确「草稿作废走软删除」。
    /// 这条断言的作用是：谁想加 `CANCELLED` 边，必须先改这张测试 + 改 DDL，
    /// 不能只改注册表（否则 DB 的 CHECK 会在写入时 500）。
    #[test]
    fn contract_has_no_cancelled_status() {
        assert!(
            !CONTRACT_STATUSES.contains(&"CANCELLED"),
            "合同表没有 CANCELLED 态（草稿作废走软删除，§4.2）"
        );
        for status in CONTRACT_STATUSES {
            assert!(
                !Entity::Contract.can_transition(status, "CANCELLED"),
                "→ CANCELLED 的边不该存在"
            );
            assert!(
                !Entity::Contract.can_transition("CANCELLED", status),
                "CANCELLED → 的边不该存在"
            );
        }
    }
}

// ==================== 人力资源状态机（HR-08 · ARCH-9 §4.2） ====================
//
// 纯函数用例：边表 / BASE / REASON / LOCKED 都是编译期常量，`can_transition()` /
// `next_statuses()` / `needs_reason()` / `is_locked()` 全是内存判定，不连库。
// 唯一读文件的地方是 `hr_status_domains_match_ddl`（读 V0383 的 `ck_biz_*_status`
// 白名单对拍）—— 这是「状态域以 DDL 为准」的机器化证明。
//
// 真库侧（`transition()` 真的写 `biz_status_log`）的用例在文件顶部 `mod tests`，
// 那组需要 `ERP_DB_URL`；本组不设环境变量也必须全绿。
#[cfg(test)]
mod hr_state_machine_tests {
    use super::*;

    /// 员工状态域（V0383 `ck_biz_employee_status`，逐字）。
    const EMPLOYEE_DOMAIN: [&str; 4] = ["PROBATION", "ACTIVE", "SUSPENDED", "RESIGNED"];
    /// 劳动合同状态域（V0383 `ck_biz_emp_contract_status`，逐字）。
    const CONTRACT_DOMAIN: [&str; 4] = ["ACTIVE", "EXPIRED", "TERMINATED", "RENEWED"];
    /// 请假单状态域（V0383 `ck_biz_leave_status`，逐字）。
    const LEAVE_DOMAIN: [&str; 4] = ["DRAFT", "SUBMITTED", "APPROVED", "CANCELLED"];
    /// 考勤期间状态域（V0383 `ck_biz_att_period_status`，逐字）。
    const ATT_DOMAIN: [&str; 3] = ["DRAFT", "SUBMITTED", "LOCKED"];
    /// 薪资单状态域（V0383 `ck_biz_sal_sheet_status`，逐字 —— **6 值，无 CANCELLED**）。
    const SALARY_DOMAIN: [&str; 6] = ["DRAFT", "CALCULATED", "SUBMITTED", "APPROVED", "POSTED", "REVERSED"];

    /// 五个 HR 实体 →（DDL 约束名，状态域）。
    fn hr_ddl_cases() -> Vec<(Entity, &'static str, Vec<&'static str>)> {
        vec![
            (Entity::Employee, "ck_biz_employee_status", EMPLOYEE_DOMAIN.to_vec()),
            (
                Entity::EmployeeContract,
                "ck_biz_emp_contract_status",
                CONTRACT_DOMAIN.to_vec(),
            ),
            (Entity::LeaveRequest, "ck_biz_leave_status", LEAVE_DOMAIN.to_vec()),
            (
                Entity::AttendancePeriod,
                "ck_biz_att_period_status",
                ATT_DOMAIN.to_vec(),
            ),
            (Entity::SalarySheet, "ck_biz_sal_sheet_status", SALARY_DOMAIN.to_vec()),
        ]
    }

    /// 读 HR 建表迁移（V0383，已应用、不可改）。
    fn hr_ddl() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/business/V0383__hr_tables.up.sql");
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
    }

    /// 从 V0383 里抠出 `CONSTRAINT <name> … CHECK (status IN ('A','B'))` 的白名单（保序）。
    ///
    /// 手写解析（不引 regex）：从约束名起找第一个 `IN (`，读到下一个 `)` 为止，
    /// 按逗号切开去引号 —— 迁移里的 `status IN (...)` 只有一个，够用且不脆。
    fn ddl_status_domain(sql: &str, constraint: &str) -> Vec<String> {
        let marker = format!("CONSTRAINT {constraint}");
        let start = sql
            .find(&marker)
            .unwrap_or_else(|| panic!("V0383 里找不到约束 {constraint}"));
        let tail = &sql[start..];
        let open = tail
            .find("IN (")
            .unwrap_or_else(|| panic!("{constraint} 里找不到 `IN (`"))
            + 4;
        let rest = &tail[open..];
        let close = rest
            .find(')')
            .unwrap_or_else(|| panic!("{constraint} 的 IN 列表没有闭合"));
        rest[..close]
            .split(',')
            .map(|item| item.trim().trim_matches('\'').to_string())
            .collect()
    }

    /// ❶ 状态域以 DDL 为准：注册表里每个实体的状态域 == V0383 的 `ck_biz_*_status`。
    ///
    /// 迁移不可改，所以这条断言是「设计 ↔ 库」的唯一裁判：设计 §4.2.4 给薪资单画了
    /// `CANCELLED`，而库层白名单里没有 —— 这里一红就说明有人照图改注册表了。
    #[test]
    fn hr_status_domains_match_ddl() {
        let sql = hr_ddl();
        for (entity, constraint, expected) in hr_ddl_cases() {
            let actual = ddl_status_domain(&sql, constraint);
            assert_eq!(actual, expected, "{:?} 的 DDL 白名单与测试期望不一致（V0383）", entity);
            assert!(!actual.is_empty(), "{constraint} 解析出空白名单");
        }
    }

    /// ❷ 边表 / BASE / REASON / LOCKED 里出现的每个状态都必须在 DDL 白名单内。
    ///
    /// 越域 = 一次 `UPDATE … SET status` 就 23514，而 `transition()` 的日志已经写下去 ——
    /// 「日志说有这一步、单据上没有这个状态」。这条断言让这种注册表改动编译期就红。
    #[test]
    fn hr_edge_statuses_stay_in_ddl_domain() {
        for (entity, _, domain) in hr_ddl_cases() {
            let in_domain = |status: &str, what: &str| {
                assert!(
                    domain.contains(&status),
                    "{:?} 的 {what} 用了 DDL 白名单外的状态「{status}」（V0383 只认 {domain:?}）",
                    entity
                );
            };
            for (from, to) in entity.edges() {
                in_domain(from, "边表");
                in_domain(to, "边表");
            }
            for edge in entity.base_edges() {
                in_domain(edge.from, "BASE_EDGES");
                in_domain(edge.to, "BASE_EDGES");
            }
            for (from, to) in entity.reason_edges() {
                in_domain(from, "REASON_EDGES");
                in_domain(to, "REASON_EDGES");
            }
            for status in entity.locked_statuses() {
                in_domain(status, "LOCKED");
            }
        }
    }

    /// ❸ 七个注册点全部生效：变体 / ALL / as_str / label / edges / BASE / REASON / LOCKED。
    ///
    /// 漏任何一处都不是「少个名字」：`transition()` 靠 `Entity::parse` 找实体，
    /// 漏登记 = 500「未在状态机注册」；漏 `base_edges()` 则配置层拿不到边。
    #[test]
    fn hr_entities_registered_in_every_place() {
        let expected: [(Entity, &str, &str); 5] = [
            (Entity::Employee, "EMPLOYEE", "员工"),
            (Entity::EmployeeContract, "EMPLOYEE_CONTRACT", "劳动合同"),
            (Entity::LeaveRequest, "LEAVE_REQUEST", "请假单"),
            (Entity::AttendancePeriod, "ATTENDANCE_PERIOD", "考勤期间"),
            (Entity::SalarySheet, "SALARY_SHEET", "薪资单"),
        ];
        for (entity, raw, label) in expected {
            assert!(
                Entity::ALL.contains(&entity),
                "{raw} 没进 Entity::ALL（transition 会 500）"
            );
            assert_eq!(entity.as_str(), raw);
            assert_eq!(entity.label(), label);
            assert_eq!(Entity::parse(raw), Some(entity));
            assert!(!entity.edges().is_empty(), "{raw} 没有边表");
            assert_eq!(
                entity.edges().len(),
                entity.base_edges().len(),
                "{raw} 的 BASE 与边表条数不同"
            );
            // 常量常量同源
            match entity {
                Entity::Employee => {
                    assert_eq!(entity.edges(), &EMPLOYEE_EDGES[..]);
                    assert_eq!(entity.reason_edges(), &EMPLOYEE_REASON_EDGES[..]);
                    assert_eq!(entity.locked_statuses(), &EMPLOYEE_LOCKED[..]);
                }
                Entity::EmployeeContract => {
                    assert_eq!(entity.edges(), &EMPLOYEE_CONTRACT_EDGES[..]);
                    assert_eq!(entity.reason_edges(), &EMPLOYEE_CONTRACT_REASON_EDGES[..]);
                    assert_eq!(entity.locked_statuses(), &EMPLOYEE_CONTRACT_LOCKED[..]);
                }
                Entity::LeaveRequest => {
                    assert_eq!(entity.edges(), &LEAVE_EDGES[..]);
                    assert_eq!(entity.reason_edges(), &LEAVE_REASON_EDGES[..]);
                    assert_eq!(entity.locked_statuses(), &LEAVE_LOCKED[..]);
                }
                Entity::AttendancePeriod => {
                    assert_eq!(entity.edges(), &ATTENDANCE_PERIOD_EDGES[..]);
                    assert_eq!(entity.reason_edges(), &ATTENDANCE_PERIOD_REASON_EDGES[..]);
                    assert_eq!(entity.locked_statuses(), &ATTENDANCE_PERIOD_LOCKED[..]);
                }
                Entity::SalarySheet => {
                    assert_eq!(entity.edges(), &SALARY_SHEET_EDGES[..]);
                    assert_eq!(entity.reason_edges(), &SALARY_SHEET_REASON_EDGES[..]);
                    assert_eq!(entity.locked_statuses(), &SALARY_SHEET_LOCKED[..]);
                }
                other => panic!("{other:?} 不是本卡注册的 HR 实体"),
            }
            // DocKind 域内（巡检按它筛实体，不在域内会把正常数据判红）
            assert!(DocKind::parse(entity.as_str()).is_some(), "{raw} 不在 DocKind 域内");
        }
    }

    /// ❹ 每条登记过的边都被接受，且 `next_statuses(from)` 恰好等于「出边目标集合」。
    #[test]
    fn hr_legal_edges_all_pass() {
        // 逐实体把「from → 允许的去向」写成表，与文档流转图对拍。
        type FlowTable = [(Entity, &'static [(&'static str, &'static [&'static str])]); 5];
        let cases: FlowTable = [
            (
                Entity::Employee,
                &[
                    ("PROBATION", &["ACTIVE", "RESIGNED"]),
                    ("ACTIVE", &["SUSPENDED", "RESIGNED"]),
                    ("SUSPENDED", &["ACTIVE", "RESIGNED"]),
                ],
            ),
            (
                Entity::EmployeeContract,
                &[
                    ("ACTIVE", &["EXPIRED", "TERMINATED", "RENEWED"]),
                    ("EXPIRED", &["RENEWED"]),
                ],
            ),
            (
                Entity::LeaveRequest,
                &[
                    ("DRAFT", &["SUBMITTED"]),
                    ("SUBMITTED", &["DRAFT", "APPROVED"]),
                    ("APPROVED", &["CANCELLED"]),
                ],
            ),
            (
                Entity::AttendancePeriod,
                &[
                    ("DRAFT", &["SUBMITTED"]),
                    ("SUBMITTED", &["LOCKED"]),
                    ("LOCKED", &["DRAFT"]),
                ],
            ),
            (
                Entity::SalarySheet,
                &[
                    ("DRAFT", &["CALCULATED"]),
                    ("CALCULATED", &["SUBMITTED"]),
                    ("SUBMITTED", &["CALCULATED", "APPROVED"]),
                    ("APPROVED", &["POSTED"]),
                    ("POSTED", &["REVERSED"]),
                ],
            ),
        ];
        for (entity, edges) in cases {
            for (from, targets) in edges {
                for to in *targets {
                    assert!(
                        entity.can_transition(from, to),
                        "{:?} 的合法边 {from} → {to} 被拒了",
                        entity
                    );
                    assert!(
                        entity.next_statuses(from).contains(to),
                        "{:?} 的 next_statuses({from}) 里应有 {to}: {:?}",
                        entity,
                        entity.next_statuses(from)
                    );
                }
                let mut got = entity.next_statuses(from);
                let mut want = targets.to_vec();
                got.sort_unstable();
                want.sort_unstable();
                assert_eq!(got, want, "{:?} 的 {from} 去向集合与文档不一致", entity);
            }
        }
    }

    /// ❺ 非法跳转一律被拒（每个实体逐条列举「最容易被误加/误点」的那些）。
    #[test]
    fn hr_illegal_edges_are_rejected() {
        let forbidden: [(Entity, &[(&str, &str)]); 5] = [
            (
                Entity::Employee,
                &[
                    // 员工不能凭空复活 / 从离职回退；试用期不经转正直接停薪
                    ("RESIGNED", "ACTIVE"),
                    ("RESIGNED", "PROBATION"),
                    ("PROBATION", "SUSPENDED"),
                    // 停薪不能直接回试用期
                    ("SUSPENDED", "PROBATION"),
                    // 自环不是状态变更
                    ("ACTIVE", "ACTIVE"),
                ],
            ),
            (
                Entity::EmployeeContract,
                &[
                    // 到期合同不能被「解除」（自然终止 ≠ 解除）
                    ("EXPIRED", "TERMINATED"),
                    // 两个终态都不许复活
                    ("TERMINATED", "ACTIVE"),
                    ("RENEWED", "ACTIVE"),
                    ("RENEWED", "EXPIRED"),
                    // 合同不能跳过状态直接封存
                    ("ACTIVE", "ACTIVE"),
                ],
            ),
            (
                Entity::LeaveRequest,
                &[
                    // 草稿作废走软删，不是 CANCELLED
                    ("DRAFT", "CANCELLED"),
                    // 审批中不能直接作废（要先驳回/撤回）
                    ("SUBMITTED", "CANCELLED"),
                    // 审批不能倒退
                    ("APPROVED", "DRAFT"),
                    ("APPROVED", "SUBMITTED"),
                    // 已作废不能复活
                    ("CANCELLED", "DRAFT"),
                    ("CANCELLED", "APPROVED"),
                ],
            ),
            (
                Entity::AttendancePeriod,
                &[
                    // 不能跳级：草稿直接锁定
                    ("DRAFT", "LOCKED"),
                    // 提交后不能自行回草稿（回位只有「解锁」一条）
                    ("SUBMITTED", "DRAFT"),
                    // 已锁定不能再提交
                    ("LOCKED", "SUBMITTED"),
                ],
            ),
            (
                Entity::SalarySheet,
                &[
                    // 试算不能跳过 CALCULATED
                    ("DRAFT", "SUBMITTED"),
                    // 没审批不能过账
                    ("CALCULATED", "APPROVED"),
                    ("CALCULATED", "POSTED"),
                    ("DRAFT", "POSTED"),
                    // 红冲是终态：设计 §4.2.4 写死「不回到 CALCULATED」
                    ("REVERSED", "CALCULATED"),
                    ("REVERSED", "POSTED"),
                    // 过账后不能退回审批前
                    ("POSTED", "CALCULATED"),
                    ("APPROVED", "CALCULATED"),
                ],
            ),
        ];
        for (entity, pairs) in forbidden {
            for (from, to) in pairs {
                assert!(!entity.can_transition(from, to), "{:?} 不该有边 {from} → {to}", entity);
            }
        }
    }

    /// ❻ 自环一律非法（任何状态 → 自己都不是「状态变更」，`transition()` 会 409）。
    #[test]
    fn hr_self_loops_are_rejected() {
        for (entity, _, domain) in hr_ddl_cases() {
            for status in domain {
                assert!(
                    !entity.can_transition(status, status),
                    "{:?} 登记了自环 {status} → {status}（原地推进要用 transition_if_changed）",
                    entity
                );
            }
        }
    }

    /// ❼ 终态没有出边（出了就不再动，要退只能红冲 / 新开单 / 走审批）。
    #[test]
    fn hr_terminal_states_have_no_outgoing_edges() {
        let terminals: [(Entity, &[&str]); 5] = [
            (Entity::Employee, &["RESIGNED"]),
            (Entity::EmployeeContract, &["TERMINATED", "RENEWED"]),
            (Entity::LeaveRequest, &["CANCELLED"]),
            (Entity::AttendancePeriod, &[]),
            (Entity::SalarySheet, &["REVERSED"]),
        ];
        for (entity, statuses) in terminals {
            for status in statuses {
                assert!(
                    entity.next_statuses(status).is_empty(),
                    "{:?} 的终态 {status} 竟然还有出边 {:?}",
                    entity,
                    entity.next_statuses(status)
                );
            }
        }
        // 考勤期间**故意是环**：`LOCKED → DRAFT`（解锁）让每个月都能重跑 —— 客户第一轮
        // 导入几乎总有错行，没有解锁就整月重来（设计 §4.2.3）。所以它没有一个「终态」，
        // 这里反过来钉住这条回位边存在，别被人当「多出来的边」删掉。
        assert!(
            Entity::AttendancePeriod.can_transition("LOCKED", "DRAFT"),
            "考勤期间必须能从 LOCKED 解锁回 DRAFT（设计 §4.2.3），否则导入错行只能整月重来"
        );
        assert_eq!(
            Entity::AttendancePeriod.next_statuses("LOCKED"),
            vec!["DRAFT"],
            "解锁是 LOCKED 唯一的出边"
        );
    }

    /// ❽ reason 边就是文档点名的那些（设计 §4.2.1~§4.2.4），且与 BASE 的
    /// `requires_reason` 逐条一致（这条同时是「撤回不能被 400 挡住」的护栏）。
    #[test]
    fn hr_reason_edges_are_exactly_the_documented_ones() {
        let expected: [(Entity, &[(&str, &str)]); 5] = [
            (
                Entity::Employee,
                &[
                    ("ACTIVE", "SUSPENDED"),
                    ("PROBATION", "RESIGNED"),
                    ("ACTIVE", "RESIGNED"),
                    ("SUSPENDED", "RESIGNED"),
                ],
            ),
            (Entity::EmployeeContract, &[("ACTIVE", "TERMINATED")]),
            (Entity::LeaveRequest, &[("APPROVED", "CANCELLED")]),
            (Entity::AttendancePeriod, &[("LOCKED", "DRAFT")]),
            (Entity::SalarySheet, &[("POSTED", "REVERSED")]),
        ];
        for (entity, pairs) in expected {
            let mut want: Vec<(&str, &str)> = pairs.to_vec();
            let mut got: Vec<(&str, &str)> = entity.reason_edges().to_vec();
            want.sort_unstable();
            got.sort_unstable();
            assert_eq!(got, want, "{:?} 的 REASON_EDGES 与文档不一致", entity);

            // BASE 的 requires_reason 必须与 REASON 表逐条同值
            for edge in entity.base_edges() {
                let needs = entity.needs_reason(edge.from, edge.to);
                assert_eq!(
                    edge.meta.requires_reason, needs,
                    "{:?} 的 {} → {} 的 BASE.requires_reason 与 REASON 表不一致",
                    entity, edge.from, edge.to
                );
            }
            // 撤回/驳回这类「同一条边两个动作」不能被标成必填原因（LNK-P08）
            for (from, to) in [("SUBMITTED", "DRAFT"), ("SUBMITTED", "CALCULATED")] {
                if entity.can_transition(from, to) {
                    assert!(
                        !entity.needs_reason(from, to),
                        "{:?} 的 {from} → {to}（撤回）不该强制原因，否则正常业务路径被 400 挡住",
                        entity
                    );
                }
            }
        }
    }

    /// ❾ 锁定态就是文档写的那几个（`ensure_writable` 的门）。
    #[test]
    fn hr_locked_statuses_are_exactly_the_documented_ones() {
        let expected: [(Entity, &[&str]); 5] = [
            (Entity::Employee, &["RESIGNED"]),
            (Entity::EmployeeContract, &["TERMINATED", "RENEWED"]),
            (Entity::LeaveRequest, &["APPROVED", "CANCELLED"]),
            (Entity::AttendancePeriod, &["LOCKED"]),
            (Entity::SalarySheet, &["SUBMITTED", "APPROVED", "POSTED", "REVERSED"]),
        ];
        for (entity, statuses) in expected {
            let mut want = statuses.to_vec();
            let mut got = entity.locked_statuses().to_vec();
            want.sort_unstable();
            got.sort_unstable();
            assert_eq!(got, want, "{:?} 的 LOCKED 与设计不一致", entity);
            for status in statuses {
                assert!(entity.is_locked(status), "{:?} 的 {status} 应为锁定态", entity);
            }
            // 草稿一定可写（每个实体都有草稿态，除了合同/员工这两个主数据）
            if entity.can_transition("DRAFT", "SUBMITTED") {
                assert!(!entity.is_locked("DRAFT"), "{:?} 的 DRAFT 不该被锁", entity);
            }
        }
    }

    /// ❿ 薪资单没有 `CANCELLED` 的边（设计 §4.2.4 的图与 §3.1 ⑧ 的 DDL 冲突的收口）。
    ///
    /// 这条断言的作用是：谁想「照图补作废」，必须先看到 DDL 的白名单里根本没有这个值 ——
    /// 补边 = 每次作废都 23514，而且 `biz_status_log` 会先记下这一步。
    /// 薪资单的退出只有一条：`SUBMITTED → CALCULATED`（驳回 / 撤回重算）。
    #[test]
    fn hr_salary_sheet_has_no_cancelled_status() {
        assert!(
            !SALARY_DOMAIN.contains(&"CANCELLED"),
            "V0383 的 ck_biz_sal_sheet_status 只有 6 值，没有 CANCELLED"
        );
        let ddl = ddl_status_domain(&hr_ddl(), "ck_biz_sal_sheet_status");
        assert!(
            !ddl.contains(&"CANCELLED".to_string()),
            "迁移里若真加了 CANCELLED，才轮到改注册表（当前 DDL：{ddl:?}）"
        );
        for (from, to) in Entity::SalarySheet.edges() {
            assert!(*to != "CANCELLED", "薪资单不该有 → CANCELLED 的边（{from} → {to}）");
            assert!(*from != "CANCELLED", "薪资单不该有 CANCELLED → 的边（{from} → {to}）");
        }
        assert!(!Entity::SalarySheet.is_locked("CANCELLED"));
        // 驳回/撤回的回位边必须存在（否则被驳回的薪资单永久卡 SUBMITTED = 死单）
        assert!(
            Entity::SalarySheet.can_transition("SUBMITTED", "CALCULATED"),
            "缺 SUBMITTED → CALCULATED：驳回后无路可走（AGENTS.md 约定 11）"
        );
    }

    /// ⓫ 非法跳转的报错文案点名「允许的去向」（前端直接展示给用户）。
    #[test]
    fn hr_rejection_message_names_next_statuses() {
        let message = rejection_message("SALARY_SHEET", "CALCULATED", "POSTED");
        assert!(message.contains("薪资单"), "文案要带单据名：{message}");
        assert!(message.contains("CALCULATED"), "文案要点名当前状态：{message}");
        assert!(message.contains("POSTED"), "文案要点名被拒的目标：{message}");
        assert!(message.contains("SUBMITTED"), "文案要给出允许的去向：{message}");

        // 终态：文案要说「已是终态」而不是列空集合
        let terminal = rejection_message("EMPLOYEE", "RESIGNED", "ACTIVE");
        assert!(terminal.contains("终态"), "终态文案要说明原因：{terminal}");
    }
}
