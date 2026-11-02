//! 调拨单数据层 + 6 态状态机（批次 D · W3b）。
//!
//! 依据：`backend/migrations/business/V0140__erp_stock_transfer.up.sql`（W3a 落地）。
//! **表结构以 `\d` 实测为准**，卡面的列清单不是权威：
//!
//! ```text
//! biz_stock_transfer            id, transfer_no, from_warehouse_id, to_warehouse_id,
//!                               transit_warehouse_id, status, transfer_date, reason,
//!                               applicant_id, applicant_name, approver_id, approval_instance_id,
//!                               total_qty, total_amount, submitted_at, approved_at, shipped_at,
//!                               received_at, cancelled_at, cancel_reason, remark,
//!                               created_at, updated_at, create_by, update_by, deleted_at
//! biz_stock_transfer_item       id, transfer_id, line_no, material_id, from_location_id,
//!                               to_location_id, qty, shipped_qty, received_qty, unit_cost,
//!                               amount, remark, created_at, updated_at, create_by, update_by
//! biz_stock_transfer_item_serial id, item_id, serial_id, created_at, create_by
//! ```
//!
//! # 三条「卡面没写、但库里有」的硬事实（照卡面写必然挂）
//!
//! 1. **`ck_biz_transfer_state` 把 6 个状态与时间戳绑死了**：
//!
//!    | 状态 | 必须非空 | 必须为空 | 额外 |
//!    |---|---|---|---|
//!    | `DRAFT` | — | 全部时间戳 | — |
//!    | `PENDING` | `submitted_at` | `approved_at`/`shipped_at`/`received_at`/`cancelled_at` | — |
//!    | `APPROVED` | `submitted_at`, `approved_at` | `shipped_at`/`received_at`/`cancelled_at` | — |
//!    | `IN_TRANSIT` | `submitted_at`, `approved_at`, `shipped_at` | `received_at`/`cancelled_at` | `transit_warehouse_id` 非空 |
//!    | `DONE` | 前四个 | `cancelled_at` | `transit_warehouse_id` 非空 |
//!    | `CANCELLED` | `cancelled_at` | — | `cancel_reason` 非空白 |
//!
//!    所以「状态流转」**不是只改一列**：每个目标状态的 `SET` 子句不同（见
//!    [`apply_status`] 里五条静态 SQL），审批驳回打回 `DRAFT` 还要把
//!    `submitted_at` / `approved_at` 清回 NULL。
//! 2. **`biz_stock_transfer_item` 没有 `deleted_at`**：改单只能「先删 serial 行、再删 item 行、
//!    再整批重插」（[`replace_items`]）。
//! 3. **`IN_TRANSIT` / `DONE` 必须有在途仓**：W3a 的 V0139 种了 `WH-TRANSIT`，
//!    发货时由 [`transit_warehouse_id`] 现查，查不到就 `400`（不能把状态改到一半）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 调拨单是仓储作业单据，V0140 的三张表都没有 dept_id / 归属人列，
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:transfer:* 权限码控制。
//! 列表分页仍走 scope::paginate_unscoped（闸①口径），不回落到框架 paginate_with_params。
//! SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置表，按 biz_type / flow_code 精确取 1 行，
//! 不参与数据范围，也不给列表分页。
//! ```
//!
//! # 事务边界
//!
//! 写函数一律吃 `&impl SqlExecutor`（`Db` 与 `Transaction` 都实现），
//! 由 [`crate::business::erp::transfer_handler`] 开事务：**建单 = 取号 + 单头 + 明细 + 序列号**、
//! **提交审批 = 状态写入 + 发起审批单 + 回写 `approval_instance_id`** 必须原子。

// ==================== 部件（TD-1 拆分，facade 重导出） ====================
//
// 本文件只做 facade：常量 / 状态机 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::erp::<本文件>::xxx` 的调用方一行不用改。

pub use super::transfer_base::*;
pub use super::transfer_query::*;
pub use super::transfer_scan::*;
pub use super::transfer_settle::*;
pub use super::transfer_write::*;

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
