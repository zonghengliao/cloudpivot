//! 安全库存 / 水位设置（M2-13）：**物料 × 仓库** 的最低 / 最高水位。
//!
//! 卡：t_d997ada8（M2-13【M2·仓库】安全库存：字段（物料 × 仓库）最低/最高水位）；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-13 行。
//!
//! ```text
//! 表（V0428）      biz_material_stock_policy   一格一行（物料 × 仓库），软删后同键可重建
//!                  ├─ min_qty  最低水位（= 该格的安全库存阈值）
//!                  └─ max_qty  最高水位（NULL = 未设 = 不判超储）
//! 设置接口         PUT    /business/erp/stock-policy          …:stock-policy:save    （upsert）
//! 列表接口         GET    /business/erp/stock-policy/page     …:stock-policy:list
//! 删除接口         DELETE /business/erp/stock-policy/{id}     …:stock-policy:delete  （软删 = 回退物料级）
//! 生效水位         model::effective_of(物料, 仓库) → EffectiveLevels（三级回退的唯一实现）
//! 结存             永远现算 biz_inventory.qty —— 设置表里不存、不缓存
//! 下游             M2-14 水位检查（低于 → 生成待办）/ M2-15 补货建议 / M2-17 前端设置页
//! ```
//!
//! # 本卡交付「字段」，不交付「检查」
//!
//! | 卡 | 做什么 | 本卡是否做 |
//! |:--|:--|:--:|
//! | M2-13（本卡） | 表 + 设置/列表/删除接口 + 生效水位的取值口径（纯函数 + 一个读数入口） | ✅ |
//! | M2-14 | 水位检查：结存 < 最低水位 → 生成待办 | ❌ |
//! | M2-15 | 补货建议：最低水位 + 在途 + 未清订单 → 建议量 | ❌ |
//! | M2-16 | 安全库存单测扩充 | ❌ |
//! | M2-17 | 前端设置页 | ❌ |
//!
//! 所以本模块**不读** `biz_inventory`、不生成任何单据 / 待办、不注册定时任务。
//!
//! # 与 `biz_material.safety_stock`（物料级）的关系
//!
//! 旧字段**不删不废**，退化成「该物料所有仓库的缺省值」：一格没有策略行（或策略停用）时，
//! 水位按物料级 `safety_stock` 判 —— `reminder_stock` 的低库存扫描（V0210 起在生产跑）
//! 今天就是这个口径，本卡不改它的行为。两者合并的**唯一实现**是 [`dto::resolve_min_qty`]：
//!
//! ```text
//! 策略行启用   → 策略行 min_qty（0 = 本格明示不预警，阻断回退）
//! 策略行停用   → 物料级 safety_stock（> 0 才算）
//! 无策略行     → 同上
//! ```
//!
//! 两份水位数据不会「打架」：策略行在语义上就是**覆盖**（override），不是叠加。
//!
//! # 四个容器文件
//!
//! * [`dto`] —— 入参 / 出参契约 + 校验 + 生效水位的纯函数（`resolve_min_qty` / `resolve_max_qty`）；
//! * [`model`] —— 数据层：分页 / 按 id / 按 物料×仓库 取行、insert / update / soft delete、
//!   以及下游唯一读数入口 `effective_of`；
//! * [`handler`] —— 接口层：3 条路由 + 参数校验 + 存在性校验 + 409/400 人话；
//! * 本文件 —— 模块地图与口径总述。
//!
//! 真库用例见 [`policy_db_tests`]（`ERP_DB_URL` 开关，与其它 ERP 用例共用同一个串行锁）。

pub mod dto;
pub mod handler;
pub mod model;

/// 水位设置的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod policy_db_tests;

pub use dto::{
    EffectiveMin, MinSource, PolicyFilter, PolicyLevels, PolicyRes, PolicyWrite, STATUS_DISABLED, STATUS_ENABLED,
    SavePolicyReq, max_qty_allowed, min_qty_allowed, resolve_max_qty, resolve_min_qty,
};
pub use handler::{
    PERM_DELETE, PERM_LIST, PERM_SAVE, STOCK_POLICY_PERMISSIONS, delete_policy_of, page_policy_of, save_policy_of,
};
pub use model::{
    EffectiveLevels, POLICY_TARGET, PolicyRow, effective_of, policy_find_by_id, policy_find_by_key, policy_page,
};
