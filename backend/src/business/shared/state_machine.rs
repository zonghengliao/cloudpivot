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
pub const DEPRECIATION_REASON_EDGES: [(&str, &str); 2] =
    [("POSTED", "REVERSED"), ("REVERSED", "CALCULATED")];

/// 折旧「不可改」状态：已过账 / 已红冲的批次行不许再改金额
/// （重跑走 `REVERSED → CALCULATED` 显式流转）。
pub const DEPRECIATION_LOCKED: [&str; 2] = ["POSTED", "REVERSED"];

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

/// 凭证没有「状态流转」：一落库就是 `POSTED`，更正靠**红冲开新单**，不原地改状态。
///
/// P-FLOW-3B 的「凭证红冲审批」**不新增边**，这是刻意的（卡面硬红线 3）：
/// 红冲的语义是「生成红字凭证、原凭证保持 POSTED」。若真的把原凭证改成
/// `PENDING_REVERSE`，四大报表 / 总账 / 余额表的取数（`WHERE v.status = 'POSTED'`）
/// 会让这张**已入账**的凭证在报表里消失 —— 审批流程不该改变账务的存在性。
/// 所以「审批中」用 `biz_voucher.pending_reverse_instance_id` 表达，不走状态机。
pub const VOUCHER_EDGES: [(&str, &str); 0] = [];

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
pub const PERIOD_EDGES: [(&str, &str); 3] = [
    ("NOT_OPENED", "OPEN"),
    ("OPEN", "CLOSED"),
    ("CLOSED", "OPEN"),
];

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
/// 需要 reason 的边：调拨单取消。
pub const TRANSFER_REASON_EDGES: [(&str, &str); 2] =
    [("DRAFT", "CANCELLED"), ("PENDING", "CANCELLED")];
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
/// 凭证没有流转边，自然也没有 reason 边。
pub const VOUCHER_REASON_EDGES: [(&str, &str); 0] = [];

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
/// 调拨单入账态：审批通过即锁定（在途 / 完成都不可改）。
pub const TRANSFER_LOCKED: [&str; 3] = ["APPROVED", "IN_TRANSIT", "DONE"];
/// 应付入账态：已结清 / 已作废都锁定（作废的单子当然不能再改）。
///
/// P-FLOW-3B 把 `PENDING_CANCEL` 也加进来：作废审批中如果还能改金额 / 备注，
/// 财务经理批的就是一份已经变过的数据（审批对象 ≠ 执行对象），复核等于没做。
pub const PAYABLE_LOCKED: [&str; 3] = ["SETTLED", "CANCELLED", "PENDING_CANCEL"];
/// 凭证入账态：已记账。
pub const VOUCHER_LOCKED: [&str; 1] = ["POSTED"];

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
pub const CONTRACT_REASON_EDGES: [(&str, &str); 2] =
    [("SIGNED", "TERMINATED"), ("ACTIVE", "TERMINATED")];

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
pub const CONTRACT_LOCKED: [&str; 5] = [
    "PENDING_APPROVAL",
    "SIGNED",
    "ACTIVE",
    "COMPLETED",
    "TERMINATED",
];

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

/// 凭证没有流转边（红冲 = 生成红字凭证，原凭证保持 `POSTED`，见 [`VOUCHER_EDGES`]）。
pub const VOUCHER_BASE: [BaseEdge; 0] = [];

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
    BaseEdge::mandatory(
        "REVERSED",
        "CALCULATED",
        "红冲后重跑（复用同一批次行）",
        true,
    ),
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
}

impl Entity {
    /// 全部实体（注册表全量，未知类型就是对表外的字符串）。
    pub const ALL: [Entity; 13] = [
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
        }
    }

    /// 按 `(from, to)` 找 BASE 边（找不到 = 这条边在代码里就不存在）。
    pub fn base_edge(self, from: &str, to: &str) -> Option<&'static BaseEdge> {
        self.base_edges()
            .iter()
            .find(|edge| edge.from == from && edge.to == to)
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
        self.reason_edges()
            .iter()
            .any(|(f, t)| *f == from && *t == to)
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
        self.edges
            .iter()
            .find(|edge| edge.from == from && edge.to == to)
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
        ex.exec_sql(format!("SAVEPOINT {SAVEPOINT}"), vec![])
            .await?;
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
        }
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
fn normalize_config_text(
    raw: Option<&String>,
    field: &str,
    max: usize,
) -> Result<Option<String>, ApiError> {
    let Some(text) = raw
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
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
                format!(
                    "写状态机配置失败（{} {from}→{to}）：{error}",
                    entity.as_str()
                ),
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
                format!(
                    "删除状态机配置失败（{} {from}→{to}）：{error}",
                    entity.as_str()
                ),
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
        if before.enabled {
            "ENABLED"
        } else {
            "DISABLED"
        },
        "DELETED",
        Some(&before),
        None,
    )
    .await?;
    Ok(true)
}

/// 列出现有配置（管理端 / 排障用；`entity = None` 表示全量）。
pub async fn list_edge_config(
    ex: &impl SqlExecutor,
    entity: Option<Entity>,
) -> DbResult<Vec<EdgeConfigRow>> {
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
            || self
                .permissions
                .iter()
                .any(|code| code == "*" || code == "*:*:*")
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
            name: if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            },
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
        let message = if entity.can_transition(from, to) {
            format!(
                "{} {} 的流转「{from} → {to}」已被状态机配置禁用（biz_flow_state_edge）：\
                 这条边在代码里合法，但管理员把它关掉了。当前允许的去向是 {}",
                entity.label(),
                no,
                match resolution.next_statuses(from).is_empty() {
                    true => "（无）".to_string(),
                    false => resolution
                        .next_statuses(from)
                        .iter()
                        .map(|status| format!("「{status}」"))
                        .collect::<Vec<_>>()
                        .join(" / "),
                }
            )
        } else {
            rejection_message_for(entity, from, to, &resolution.next_statuses(from))
        };
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
// 与 `transition` 同一组参数（实体 + 单据 + 状态 + 操作人 + 原因 + 关联单据），
// 两者签名刻意保持一致，调用方才能在「状态变了」与「状态没变」之间一处切换。
#[allow(clippy::too_many_arguments)]
pub async fn log_event(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    status: &str,
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
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let (ref_type, ref_no) = match biz_ref {
        Some((kind, no)) => (kind.trim().to_string(), no.trim().to_string()),
        None => (String::new(), String::new()),
    };
    ex.exec_sql(
        "INSERT INTO biz_status_log \
            (id, entity_type, entity_id, entity_no, from_status, to_status, \
             operator_id, operator_name, reason, biz_ref_type, biz_ref_no, created_at) \
         VALUES (?, ?, ?::uuid, ?, NULLIF(?::text,''), ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
                 NULLIF(?::text,''), ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(entity_type.to_string()),
            Value::from(&entity_id),
            Value::Text(entity_no.to_string()),
            Value::Text(from_status.unwrap_or_default().to_string()),
            Value::Text(to_status.to_string()),
            Value::Text(
                operator
                    .id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(operator.name.clone().unwrap_or_default()),
            Value::Text(reason.unwrap_or_default().to_string()),
            Value::Text(ref_type),
            Value::Text(ref_no),
            Value::from(Utc::now()),
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
    let label = Entity::parse(entity_type)
        .map(|item| item.label())
        .unwrap_or("单据");
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
            "SELECT id, entity_type, entity_id, entity_no, from_status, to_status, operator_id, \
                    operator_name, reason, biz_ref_type, biz_ref_no, created_at \
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
        db().execute_with_params(
            "DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SM-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理状态日志失败：{err}"));
        db().execute_with_params(
            "DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SMV-%'",
            vec![],
        )
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
        assert!(
            err.msg.contains(ENTITY_PURCHASE_ORDER),
            "要点名注册表：{}",
            err.msg
        );
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
        for (no, to) in [
            (&no_a, "SUBMITTED"),
            (&no_a, "CLOSED"),
            (&no_b, "SUBMITTED"),
        ] {
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
        assert!(
            err.msg.contains("DRAFT") && err.msg.contains("RECEIVED"),
            "{}",
            err.msg
        );
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

        db().execute_with_params(
            "DELETE FROM biz_voucher WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
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
        assert!(
            !resolution.allows("DRAFT", "POSTED"),
            "禁用的可选边不能出现在有效边里"
        );
        assert!(
            resolution.allows("DRAFT", "PENDING_POST"),
            "同状态的其他边不受影响"
        );
        assert_eq!(
            resolution.edges.len(),
            STOCK_IN_BASE.len() - 1,
            "只少了这一条"
        );
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
            resolution
                .warnings
                .iter()
                .any(|warning| warning.contains("mandatory")),
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
        assert!(
            !resolution.allows("CANCELLED", "POSTED"),
            "不能改方向 / 不能从终态复活"
        );
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
        assert_eq!(
            resolution.get("DRAFT", "CANCELLED").unwrap().label,
            "草稿作废"
        );
    }

    /// ❽ 只许加严：配置把「代码要求原因」的边写成 `requires_reason = false` 也照样要求。
    #[test]
    fn requires_reason_cannot_be_weakened() {
        let mut row = d1_row(ENTITY_STOCK_IN, "POSTED", "CANCELLED", true);
        row.requires_reason = false;
        let resolution = resolve_edges(Entity::StockIn, &[row]);
        assert!(
            resolution
                .get("POSTED", "CANCELLED")
                .unwrap()
                .requires_reason,
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
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .unwrap_err();
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
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("硬约束边"), "{}", err.msg);

        // ⑤ 把「代码要求原因」的边放宽
        let patch = d1_patch(ENTITY_STOCK_IN, "POSTED", "CANCELLED");
        let err = upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .unwrap_err();
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
        let row = upsert_edge_config(
            &tx,
            &d1_super_claims(),
            &d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED"),
        )
        .await
        .expect("超管 + confirm 应写入成功");
        assert!(row.enabled);

        let listed = list_edge_config(&tx, Some(Entity::StockIn))
            .await
            .expect("读配置");
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

    /// ⓫ 禁用 optional 边后，**真走这条边** → 409「已被状态机配置禁用」；删配置后恢复。
    #[tokio::test]
    async fn disabled_edge_is_rejected_then_restored_after_delete() {
        let Some((_guard, tx)) = d1_tx().await else {
            return;
        };
        let no = format!("{PREFIX}D1A");
        let mut patch = d1_patch(ENTITY_STOCK_IN, "DRAFT", "POSTED");
        patch.enabled = false;
        upsert_edge_config(&tx, &d1_super_claims(), &patch)
            .await
            .expect("禁用可选边应成功");

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
            err.msg.contains("已被状态机配置禁用"),
            "文案要说清是配置禁的，不是代码里没有：{}",
            err.msg
        );

        // 删配置 → 回到 BASE 行为
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
            resolution
                .warnings
                .iter()
                .any(|warning| warning.contains("mandatory")),
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
        let trail = trail_of_no(&tx, &no)
            .await
            .map(|rows| rows.len())
            .unwrap_or(0);
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
        assert_eq!(
            resolution.get("DRAFT", "PENDING_POST").unwrap().label,
            "报批"
        );
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
            13,
            "注册表从 7 → 8（CONTRACT）→ 9（STOCK_OUT）→ 10（STOCK_TAKE）→ 11（BUDGET，FIN-1）\
             → 12（FIXED_ASSET，FIN-2）→ 13（DEPRECIATION，FIN-2）"
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
            assert!(
                seen.insert(entity.as_str()),
                "实体类型串重复：{}",
                entity.as_str()
            );
        }
        let mut labels = std::collections::BTreeSet::new();
        for entity in Entity::ALL {
            assert!(
                labels.insert(entity.label()),
                "中文名重复：{}",
                entity.label()
            );
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
            assert!(
                Entity::Contract.can_transition(from, to),
                "合法边 {from} → {to} 被拒了"
            );
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
        assert_eq!(
            Entity::Contract.next_statuses("SIGNED"),
            vec!["ACTIVE", "TERMINATED"]
        );
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
        let has_incoming =
            |target: &str| Entity::Contract.edges().iter().any(|(_, to)| *to == target);
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
            assert!(
                !Entity::Contract.needs_reason(from, to),
                "{from} → {to} 不该强制填原因"
            );
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
        for status in [
            "PENDING_APPROVAL",
            "SIGNED",
            "ACTIVE",
            "COMPLETED",
            "TERMINATED",
        ] {
            assert!(Entity::Contract.is_locked(status), "{status} 应判为已冻结");
        }
        // locked 只能取状态白名单里的值（写错值等于永不生效）
        for status in Entity::Contract.locked_statuses() {
            assert!(
                CONTRACT_STATUSES.contains(status),
                "locked 里的 {status} 不是合同状态"
            );
        }
    }

    /// 非法流转的文案：点名「合同」+ 给出允许去向（前端直接展示）。
    #[test]
    fn contract_rejection_message_names_entity_and_targets() {
        let msg = rejection_message("CONTRACT", "DRAFT", "ACTIVE");
        assert!(msg.contains("合同"), "文案要带单据中文名：{msg}");
        assert!(msg.contains("DRAFT"), "文案要带当前态：{msg}");
        assert!(msg.contains("ACTIVE"), "文案要带被拒的目标态：{msg}");
        assert!(
            msg.contains("PENDING_APPROVAL"),
            "文案要给出合法去向：{msg}"
        );
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
