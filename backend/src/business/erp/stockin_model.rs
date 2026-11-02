//! 收货入库单数据层 + 过账（移动加权 / 一物一码）（批次 D · P2-B1）。
//!
//! 依据：`backend/migrations/business/V0145__purchase_order_amounts_and_stock_in.up.sql`（P2-A1 建的两张表）
//! 与 `docs/ERP-批次1-产品需求PRD.md` §3.2（成本核算，380-470 行）。
//! **表结构以 `\d` 实测为准**（2026-10-31 开工实测 `cloudpivot_dev`，已到 V0146）：
//!
//! ```text
//! biz_stock_in       id, in_no, in_type(PURCHASE/RETURN/OTHER), source_order_id, supplier_id,
//!                    warehouse_id(NOT NULL), in_date, status(DRAFT/POSTED/CANCELLED),
//!                    currency_code(CHAR(3)), total_qty, total_amount, total_tax, total_with_tax,
//!                    invoice_received, voucher_id, payable_id, remark, posted_at, posted_by,
//!                    cancelled_at, cancelled_by, cancel_reason,
//!                    created_at, updated_at, create_by, update_by, deleted_at
//! biz_stock_in_item  id, stock_in_id, line_no, source_item_id, material_id, unit_id(NOT NULL),
//!                    qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount,
//!                    cost_price, location_id, lot_no, remark, created_at, updated_at
//! biz_inventory      id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost(18,6),
//!                    amount(18,4), last_txn_at, …
//! biz_inventory_txn  22 列（qty > 0 禁负数、txn_type 只认 IN/OUT、period_code NOT NULL）
//! ```
//!
//! # 卡面没写、但库里有约束的六处（照卡面写必然挂）
//!
//! 1. **明细 `unit_id` 是 `NOT NULL`**：卡面 body 里没有这个字段 → 由物料的基础单位带出，不让客户端传。
//! 2. **`biz_stock_in_item` 的 `ck_biz_si_item_tax` 是精确恒等**（`with_tax_amount = amount + tax_amount`），
//!    回写的三金额必须逐行算好，不能靠库里补。
//! 3. **`ck_biz_si_item_qty CHECK (qty > 0)`**：0 数量的行插不进库 → 建单即 400（与 P2-A2 同口径）。
//! 4. **`ck_biz_si_supplier`**：`in_type='PURCHASE'` 时 `supplier_id` 必填（本卡只产生 `PURCHASE`）。
//! 5. **`biz_inventory` 的唯一键是表达式索引** `uk_biz_inv_mat_wh_loc
//!    (material_id, warehouse_id, COALESCE(location_id, '000…0'::uuid))` → 所有 upsert 的
//!    `ON CONFLICT` 目标必须**写成同一个表达式**，否则真库 42P10（W3a 实测）。
//! 6. **`biz_inventory_txn` 有 `period_code`（`yyyy-MM`）与 `after_*` 三列**：卡面只要求 `before_*` 快照，
//!    但列是 `NOT NULL`，`after_*` 用 upsert 回读的真实落库值填（不是「before ± 数量」算出来的）。
//!
//! # 过账口径（PRD §3.2.3 / §3.2.6，本卡核心）
//!
//! ```text
//! new_qty    = old_qty + in_qty
//! new_amount = old_amount + in_amount      ← 加法，金额是权威值，绝不用「单价×数量」重算
//! new_avg    = round(new_amount / new_qty, 6)  (new_qty > 0)
//! 结存 ≤ 0 时入库：new_avg = 本次入库单价（旧成本基准已失效，PRD §3.2.6 边界表）
//! 加锁：SELECT … FOR UPDATE，**多行按 material_id 升序**（防两个收货单互相死锁）
//! ```
//!
//! **入库成本用不含税金额**（PRD §3.2：进项税是价外可抵扣的，不进存货成本）——
//! `amount` 列与 `biz_inventory_txn.amount` 都只加 `biz_stock_in_item.amount`（不含税），
//! 不是 `with_tax_amount`。`cost_price` 落不含税单价。
//!
//! # 与卡面 §三 e) 的一处**实现差异**（有意为之，写在这里免得下游踩坑）
//!
//! 卡面写「一物一码物料调用 `serial_model::batch_generate`」。实测该函数（W2b/W2c-1）内部
//! 会走 `link_inventory` → `recompute_quantity`（把 `qty` **重算成序列号件数**）并**再写一条
//! `SERIAL_IN` 流水**。若照卡面调用，同一张收货单会：
//!
//! * 写出两条 `IN` 流水（`PURCHASE_IN` + `SERIAL_IN`）→ 台账双计；
//! * 被 `recompute_quantity` 把 `amount` 改写成 `qty × avg_cost`（`avg_cost` 还是 0）
//!   → **过账算出来的移动加权金额被静默抹掉**。
//!
//! 所以本模块只复用 `serial_model` 的**取号原语**（[`crate::business::erp::serial_model::take_numbers`]、
//! [`crate::business::erp::serial_model::format_serial_no`]、
//! [`crate::business::erp::serial_model::pair_payload`]）自己落序列号行
//! （`source_type='PURCHASE'`、`source_id=收货单 id`），库存侧只有本模块**一处**加法 +
//! **一条** `PURCHASE_IN` 流水。不变式「一物一码物料 `qty` = 在库序列号件数」仍然成立
//! （本次入库的增量两边相等），且成本链不被破坏。
//!
//! # 作废（`POSTED → CANCELLED`）本卡**只改状态**
//!
//! 卡面 §二 明写「作废需红冲 —— 但凭证是 P2-C1，本卡先只改状态」。所以
//! [`apply_status`] 在 `POSTED → CANCELLED` 时**不回滚库存**，只写状态与 `cancel_reason`，
//! 并打一条 WARN 日志点名「库存未冲回，待 P2-C1 红冲」。这条欠账写在证据文档里。
//!
//! # P6：过账同时写**两本账**（成本账 + 库位账）
//!
//! P2-B1 的过账只写成本账（`biz_inventory`，含 `location_id` 维度）与流水，
//! `biz_inventory_location`（数量账 / 库位账）**一行不写** —— 结果是
//! 「即时库存」Tab（成本账）与「库位库存」Tab（库位账）永远对不上，违反不变式③
//! `SUM(库位账 qty) = 成本账 qty`，后续按库位出库 / 盘点 / 调拨都查不到这批货。
//!
//! 现在一条明细落地**两笔**，同一个 `ex`（同一个事务）：
//!
//! ```text
//! ③  成本账：apply_cost       （biz_inventory，material + warehouse + location 三列唯一键）
//! ③b 库位账：apply_location_qty（biz_inventory_location，同一把锁顺序）
//! ```
//!
//! `location_id` 的两处语义不要混（见 [`apply_location_qty`] 文档）：
//! 明细上的 `location_id` 是 **`biz_inventory_location.id`**，而 `biz_inventory_location.location_id`
//! 列存的是 `biz_location.id`。所以「有库位」走按主键 UPDATE，「无库位」走 NULL 桶 upsert。
//!
//! **存量数据不补**：本卡只保证「新单正确」（增量修复）。已过账的历史收货单没有库位账行，
//! 差异留给人决策后再单独开卡补，绝不在迁移或代码里批量 INSERT 历史数据。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 收货入库单是 ERP 单据表（V0145 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:stock-in:* 权限码控制。
//! 列表分页走 scope::paginate_unscoped（闸①口径）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
//! SCOPE-EXEMPT: biz_inventory / biz_inventory_txn 是仓储台账，无归属列（口径同 erp/model.rs）。
//! ```
//!
//! # 事务边界
//!
//! 写函数一律吃 `&impl SqlExecutor`（`Db` / `Transaction` 都实现），由
//! [`crate::business::erp::stockin_handler`] 开事务：**过账的「改状态 + 加减库存 + 写流水 +
//! 生成序列号 + 回写采购单已收数量」必须在同一个事务里**（硬红线 2）。

// ==================== 部件（TD-1 拆分，facade 重导出） ====================
//
// 本文件只做 facade：常量 / 状态机 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::erp::<本文件>::xxx` 的调用方一行不用改。

pub use super::stockin_base::*;
pub use super::stockin_cancel::*;
pub use super::stockin_purchase::*;
pub use super::stockin_query::*;
pub use super::stockin_validate::*;
pub use super::stockin_write::*;

#[cfg(test)]
#[path = "stockin_tests.rs"]
mod tests;
