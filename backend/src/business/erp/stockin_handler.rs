//! 收货入库单接口层（批次 D · P2-B1）：建 / 改 / 状态流转（过账）/ 删 / 列表 / 详情。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/stock-in/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/stock-in` | `business:erp:stock-in:create` | 建单（`DRAFT` + `IN-yyyyMMdd-000001`，金额服务端算） |
//! | POST | `/business/erp/stock-in/{id}/copy` | `business:erp:stock-in:create` | **复制为新草稿**（D9：原单只读，新号 / `DRAFT` / 采购关联清空） |
//! | PUT | `/business/erp/stock-in/{id}` | `business:erp:stock-in:update` | 改单（**仅 `DRAFT`**） |
//! | PATCH | `/business/erp/stock-in/{id}/status` | `business:erp:stock-in:status` | `DRAFT→POSTED`（过账，**核心**）/ `→CANCELLED` |
//! | DELETE | `/business/erp/stock-in/{id}` | `business:erp:stock-in:delete` | 软删（**仅 `DRAFT`**） |
//! | GET | `/business/erp/stock-in/page` | `business:erp:stock-in:list` | 分页（关键词 / 状态 / 供应商 / 仓库 / 日期区间） |
//! | GET | `/business/erp/stock-in/{id}` | `business:erp:stock-in:detail` | 详情（单头 + 明细 + 序列号件数） |
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库 +
//! 过账」收进 [`create_stock_in`] / [`update_stock_in`] / [`change_stock_in_status`] /
//! [`delete_stock_in`] 之后，单测走**同一条代码路径**（只跳过认证）。
//!
//! # ★ 过账（硬红线 2 / 3 / 4 / 7）
//!
//! 一个事务里做完六件事（顺序即代码顺序）：
//!
//! ```text
//! ① 锁单头 SELECT … FOR UPDATE → 状态必须 DRAFT，否则 409（重复过账 = 409，幂等）
//! ①b DC-03：带来源采购单时**再过一遍**来源单闸 —— 锁采购单头（FOR UPDATE）→ 状态复核
//!    （同一 `purchase_order_fault` 判据，出口 409 + 66031）→ 本次过账数量 ≤ 可用待收量
//!    （口径 = `POSTED_RECEIVED_SQL`，出口 409 + 66032）。在扣减库存之前，失败整笔回滚。
//! ② CAS 写状态 DRAFT → POSTED（posted_at / posted_by）
//! ③ 明细**按 (material_id, location_id, line_no) 升序**逐行：库存加法 + 库位账加法 + 写 PURCHASE_IN 流水
//!    · 加锁用 SELECT … FOR UPDATE（同一物料同一库位并发串行化，before_* 快照不打架）
//!    · new_amount = old_amount + 不含税金额；new_avg = ROUND(new_amount / new_qty, 6)（SQL 侧算）
//!    · P6 补口：同一行还要把 qty 加到 `biz_inventory_location`（成本账 + 库位账两本账同事务，
//!      见 [`stockin_model::apply_location_qty`]；「有库位」按行 id UPDATE、「无库位」加 NULL 桶）
//! ④ 一物一码物料生成 / 落序列号（数量 = 本行 qty）
//! ⑤ 采购明细「已收数量」**不回写**（P2-3）：它是派生值（只算已过账收货单），
//!    本单 ② 步已进 POSTED 集合，聚合自然计入；超收闸只在 ①b `guard_purchase_before_post`
//! ⑥ 推送采购单状态 PARTIAL_RECEIVED / RECEIVED
//! ```
//!
//! ⚠️ 成本口径：`biz_inventory.amount` 与流水 `amount` 只加**不含税金额**
//! （PRD §3.2：进项税价外可抵扣，不进存货成本）。
//!
//! # 与卡面的一处入参差异（`serial_ids` 在过账时提交）
//!
//! `biz_stock_in_item` **没有存序列号的列**，硬红线 5 又不许改 V0137-V0145。所以：
//!
//! * 建单 / 改单的 `items[].serial_ids` 会被**校验**（条数必须 = qty、单行内不重复、≤64 字），
//!   但**不落库**（卡面 ⑩ 的 400 在建单就生效）；
//! * 真正落库的序列号在 `PATCH …/status` 的 body 里给：
//!   `{"status":"POSTED","serials":[{"line_no":1,"serial_ids":["SN-...","SN-..."]}]}`；
//! * 不给 = 由系统按 `SN-yyyyMMdd-NNNNNN` 取号生成（卡面 ③ 的默认路径）。
//!
//! 这条差异写进证据文档与看板：想「建单时贴码、过账时自动用」需要给明细表加一列（属后续迁移）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 收货入库单是 ERP 单据表（V0145 建表，无 dept_id / 归属人列），
//! 可见性由 business:erp:stock-in:* 权限码控制；分页走 scope::paginate_unscoped（闸①口径）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
//! ```

// ==================== 部件（TD-2 拆分，facade 重导出） ====================
//
// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。

pub use super::stockin_handler_common::*;
// 注意：`stockin_handler_purchase`（采购联动）里**一个 `pub` 项都没有**，全是内部辅助，
// 拆分前也是模块私有 → 不进 facade 重导出（跨部件调用走各部件自己的
// `use super::stockin_handler_purchase::*;`）。对外可见性与拆分前一致。
pub use super::stockin_handler_cancel::*;
pub use super::stockin_handler_post::*;
pub use super::stockin_handler_query::*;
pub use super::stockin_handler_status::*;
pub use super::stockin_handler_write::*;

// ==================== 测试（#[path] 挂到各测试部件） ====================
#[cfg(test)]
#[path = "stockin_handler_tests_fixtures.rs"]
mod stockin_handler_tests_fixtures;

#[cfg(test)]
#[path = "stockin_handler_tests_helpers.rs"]
mod stockin_handler_tests_helpers;

#[cfg(test)]
#[path = "stockin_handler_tests_posting.rs"]
mod stockin_handler_tests_posting;

#[cfg(test)]
#[path = "stockin_handler_tests_update.rs"]
mod stockin_handler_tests_update;

#[cfg(test)]
#[path = "stockin_handler_tests_location.rs"]
mod stockin_handler_tests_location;

/// P2-4：库位做完整（指定库位 / 默认库位 / 跨仓库被拒 / 容量预警 / 出库 FIFO）。
#[cfg(test)]
#[path = "stockin_handler_tests_location_dict.rs"]
mod stockin_handler_tests_location_dict;

#[cfg(test)]
#[path = "stockin_handler_tests_cancel.rs"]
mod stockin_handler_tests_cancel;

#[cfg(test)]
#[path = "stockin_handler_tests_p10_locked.rs"]
mod stockin_handler_tests_p10_locked;

#[cfg(test)]
#[path = "stockin_handler_tests_dc03.rs"]
mod stockin_handler_tests_dc03;

#[cfg(test)]
#[path = "stockin_handler_tests_p16.rs"]
mod stockin_handler_tests_p16;

#[cfg(test)]
#[path = "stockin_handler_tests_d4_purchase.rs"]
mod stockin_handler_tests_d4_purchase;

#[cfg(test)]
#[path = "stockin_handler_tests_pflow3a.rs"]
mod stockin_handler_tests_pflow3a;

#[cfg(test)]
#[path = "stockin_handler_tests_d9_copy.rs"]
mod stockin_handler_tests_d9_copy;

#[cfg(test)]
#[path = "stockin_handler_tests_a4.rs"]
mod stockin_handler_tests_a4;
