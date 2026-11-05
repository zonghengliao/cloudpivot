//! 调拨单接口层（批次 D · W3b）：7 个接口 + 6 态状态机 + 审批接入。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/transfer/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/transfer/page` | `business:erp:transfer:list` | 分页（状态 / 两仓 / 单号） |
//! | GET | `/business/erp/transfer/{id}` | `business:erp:transfer:detail` | 详情（含明细 + 每行序列号 id） |
//! | POST | `/business/erp/transfer` | `business:erp:transfer:create` | 建单（`DRAFT` + `TK-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/transfer/{id}` | `business:erp:transfer:update` | 改单（**仅 `DRAFT`**） |
//! | PATCH | `/business/erp/transfer/{id}/status` | `business:erp:transfer:status` | 状态流转（6 态） |
//! | DELETE | `/business/erp/transfer/{id}` | `business:erp:transfer:delete` | 软删（仅 `DRAFT` / `CANCELLED`） |
//! | GET | `/business/erp/transfer/{id}/serials` | `business:erp:transfer:detail` | 涉及序列号（按明细分组） |
//!
//! 路由顺序说明：`/transfer/page` 是**静态段**、`/transfer/{id}` 是动态段，axum 的 matchit
//! 静态优先，`GET /transfer/page` 不会被 `{id}` 吃掉（W2b 的 `/serial/page` 同款实测）。
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库」
//! 收进 `create_transfer` / `update_transfer` / `change_transfer_status` / `delete_transfer`
//! 之后，单测走的是**同一条代码路径**（只跳过认证），比另写一份测试逻辑更能钉住行为。
//!
//! # 审批接入（卡面 §三）
//!
//! ```text
//! DRAFT → PENDING：查已发布流程（biz_type='STOCK_TRANSFER' 或 flow_code='stock_transfer'）
//!   · 有  → 同一事务里 engine::start 发起审批单 + 回写 approval_instance_id
//!   · 没有 → 只把状态推到 PENDING（降级：PENDING → APPROVED 允许人工推）
//! 审批结论回写（flow::task_bridge::on_transfer_settled，见该函数）：
//!   · APPROVED → 调拨单 APPROVED
//!   · REJECTED → 调拨单打回 DRAFT（审批意见写 reason，时间戳清空以满足 ck_biz_transfer_state）
//! ```
//!
//! **发起审批与状态写入同事务**：审批单建不出来就整笔回滚（否则会出现「调拨单已待审批、
//! 审批单没建、谁也不知道该等谁」这种比不接审批更坏的状态）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 调拨单是仓储作业单据（V0140 三张表都没有 dept_id / 归属人列），
//! 可见性由 business:erp:transfer:* 权限码控制；列表分页走 model 的 scope::paginate_unscoped。
//! ```
//!
//! # 库存双写（W3c）
//!
//! `IN_TRANSIT → DONE` 这一步在**同一个事务**里多做四件事（[`transfer_model::settle_transfer`]）：
//!
//! ```text
//! ① 调出仓 qty -= 数量（条件扣减 AND qty >= ?，防并发超卖）+ 流水 OUT / TRANSFER_OUT
//! ② 调入仓 qty += 数量（无库存行则建档）+ 流水 IN / TRANSFER_IN
//! ③ 一物一码物料：序列号 warehouse_id / location_id 跟着走（数量物料一个序列号都不动）
//! ④ 成本原价转移：unit_cost = 调出仓 avg_cost 快照；调入仓移动加权重算；金额守恒
//! ```
//!
//! 结算失败（库存不足 / 序列号不符 / 不变式不满足）→ **整笔回滚**，调拨单仍是 `IN_TRANSIT`，
//! 不会出现「状态已 DONE、库存没动」这种对不上的账。状态 CAS（`WHERE status = 'IN_TRANSIT'`）
//! 先执行，所以同一张单被并发推两次时只有一个事务能走到结算（另一个拿 409），
//! 单号级别的重复调用也不会二次扣库存（`settle_transfer` 还有一道流水幂等闸）。

// ==================== 部件（TD-2 拆分，facade 重导出） ====================
//
// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。

// `transfer_handler_common` 里的条目拆分前是模块私有（同文件内被各部件与测试引用），
// 因此这里用 `pub(super) use` 保持**与拆分前一致的可见性**（写 `pub use` 会拉响
// `glob import doesn't reexport anything with visibility pub`：底座里没有能导出到 pub 的条目）。
// 只有测试部件经由 facade 的 `use super::*` 用到这些名字 → 只在测试构建里重导出。
#[cfg(test)]
pub(super) use super::transfer_handler_common::*;
pub use super::transfer_handler_create::*;
pub use super::transfer_handler_delete::*;
pub use super::transfer_handler_query::*;
pub use super::transfer_handler_status::*;
// 测试支撑部件本身是 `#[cfg(test)] mod`，重导出同样只在测试构建里成立。
#[cfg(test)]
pub(super) use super::transfer_handler_test_support::*;

// ==================== 测试（#[path] 挂到各测试部件） ====================
// 顺序即模块名，`cargo test --lib business::erp::transfer_handler` 会按模块名排序执行：
// `db_status` 里放着收尾的 `zz_teardown_purges_fixture_residue`，db_* 里它仍排最后。
/// 单测（不连库）：权限码导出与审批冲突 409 文案。
#[cfg(test)]
#[path = "transfer_handler_tests.rs"]
mod tests;

/// 真库用例的公共夹具（连接 / 串行锁 / 造单小工具 / 清理）。
#[cfg(test)]
#[path = "transfer_handler_db_tests_fixtures.rs"]
mod db_tests_fixtures;

/// 真库：建单 / 改单 / 复制 / 校验。
#[cfg(test)]
#[path = "transfer_handler_db_tests_create.rs"]
mod db_create;

/// 真库：六态状态机（审核 / 发出 / 在途 / 收货 / 作废）+ 审批发起与回调。
#[cfg(test)]
#[path = "transfer_handler_db_tests_status.rs"]
mod db_status;

#[cfg(test)]
#[path = "transfer_handler_db_tests_settle.rs"]
mod db_settle;

#[cfg(test)]
#[path = "transfer_handler_db_tests_books.rs"]
mod db_books;

#[cfg(test)]
#[path = "transfer_handler_db_tests_query.rs"]
mod db_query;
