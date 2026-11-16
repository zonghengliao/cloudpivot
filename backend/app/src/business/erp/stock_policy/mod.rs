//! 安全库存 / 水位：**物料 × 仓库** 的最低 / 最高水位设置（M2-13）+ **水位检查**（M2-14）。
//!
//! 卡：t_d997ada8（M2-13 安全库存：字段（物料 × 仓库）最低/最高水位）、
//! t_df0d7d48（M2-14 安全库存：水位检查（低于 → 生成待办））；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-13 / M2-14 行。
//!
//! ```text
//! 表（V0428）      biz_material_stock_policy   一格一行（物料 × 仓库），软删后同键可重建
//!                  ├─ min_qty  最低水位（= 该格的安全库存阈值）
//!                  └─ max_qty  最高水位（NULL = 未设 = 不判超储）
//! 设置接口         PUT    /business/erp/stock-policy          …:stock-policy:save    （upsert）
//! 批量设置（M2-17）PUT    /business/erp/stock-policy/batch    …:stock-policy:save    （笛卡尔积，一个事务）
//! 导入（M2-17）    POST   /business/impexp/import/upload?biz_type=business.erp.stock-policy
//!                  （impexp 通用三段：模板 → 上传预检 → 确认入库；定义在
//!                   `impexp::builtin::erp_setting`，本模块不复制 xlsx 解析 / 校验）
//! 列表接口         GET    /business/erp/stock-policy/page     …:stock-policy:list
//! 删除接口         DELETE /business/erp/stock-policy/{id}     …:stock-policy:delete  （软删 = 回退物料级）
//! 检查接口（M2-14） POST   /business/erp/stock-policy/check    …:stock-policy:check   （手动触发 / dry_run）
//! 定时任务（M2-14） erp_stock_water_check  每小时 :07 UTC，低水位 → 生成待办、回升 → 自动收口
//! 页面（M2-17）    /business/erp/stock-policy   显形码 business:erp:stock-policy:page（V0431）
//! 生效水位         dto::resolve_min_qty / resolve_max_qty（三级回退的唯一实现）
//!                  model::effective_of(物料, 仓库) → EffectiveLevels（读一格）
//! 结存             永远现算 biz_inventory.qty —— 设置表里不存、不缓存
//! 待办载体         biz_task（doc_type = INVENTORY，doc_id = biz_inventory.id）
//! ```
//!
//! # 卡的分工（别把 M2-15 的活干进来）
//!
//! | 卡 | 做什么 | 状态 |
//! |:--|:--|:--:|
//! | M2-13 | 表 + 设置/列表/删除接口 + 生效水位的取值口径（纯函数 + 一个读数入口） | ✅ |
//! | M2-14 | 水位检查：结存 < 最低水位 → **生成待办**；回升 → **自动收口** | ✅ |
//! | M2-15 | 补货建议：最低水位 + 在途 + 未清订单 → 建议量 | ✅（`replenish` 模块） |
//! | M2-16 | 安全库存单测扩充 | ✅ |
//! | M2-17（本卡） | **前端设置页 + 批量设置接口 + Excel 导入定义** | ✅ |
//!
//! M2-17 只加三样，**不动水位口径**：批量设置（[`handler::save_policy_batch_of`]，笛卡尔积
//! 一个事务）、单格与批量共用的四列校验（[`dto::checked_write`]）、Excel 导入定义
//! （`impexp::builtin::erp_setting::StockPolicyImport`，走通用导入向导）。
//! `resolve_min_qty` / `resolve_max_qty` / `effective_of` 一行没改 —— 页面批量写进去的值，
//! 与 M2-14 判水位、M2-15 给补货建议读到的值，仍是同一个口径。
//!
//! 所以本模块**不建采购单 / 调拨单**（M2-15 的补货建议才给建议量），
//! 不算「超储」（那是 M2-15 的 `max_qty` 消费者）。M2-14 只做两件事：**发待办**、**收待办**。
//!
//! # 与 `biz_material.safety_stock`（物料级）的关系
//!
//! 旧字段**不删不废**，退化成「该物料所有仓库的缺省值」：一格没有策略行（或策略停用）时，
//! 水位按物料级 `safety_stock` 判。两者合并的**唯一实现**是 [`dto::resolve_min_qty`]：
//!
//! ```text
//! 策略行启用   → 策略行 min_qty（0 = 本格明示不预警，阻断回退）
//! 策略行停用   → 物料级 safety_stock（> 0 才算）
//! 无策略行     → 同上
//! ```
//!
//! 两份水位数据不会「打架」：策略行在语义上就是**覆盖**（override），不是叠加。
//! M2-14 的水位检查读的也是这一个函数（[`water_check::WaterCell::judge`]），
//! 所以「设置页显示 100、待办按 50 报警」在结构上不可能发生。
//!
//! # 与 `reminder_stock`（低库存**通知**）的关系
//!
//! 两条通道**并存、不合并**（卡面不夹带）：
//!
//! | 通道 | 载体 | 收件人 | 口径 |
//! |:--|:--|:--|:--|
//! | `reminder_stock`（V0210 起在生产跑） | 站内通知 `sys_notice` | 仓管主管角色的每个成员 | 只用物料级 `safety_stock` |
//! | `water_check`（本卡） | 待办 `biz_task`（有负责人、有截止日、能标进度） | 仓管主管角色里**确定性第一位** | 三级回退（含每格设置） |
//!
//! 通知是「告诉你一声」，待办是「这件事有人负责」——两者语义不同，硬合并会丢掉其中一个。
//! 口径差异（`safety_stock` vs 三级回退）是既有事实，不是本卡引入的：改成策略感知属于
//! `reminder_stock` 自己的卡，本卡不动它的行为。
//!
//! # 容器文件
//!
//! * [`dto`] —— 入参 / 出参契约（含 M2-14 的 `WaterCheckReq` / `WaterCheckRes`）+ 校验 +
//!   生效水位的纯函数（`resolve_min_qty` / `resolve_max_qty`）；
//! * [`model`] —— 数据层：分页 / 按 id / 按 物料×仓库 取行、insert / update / soft delete、
//!   以及读一格生效水位的入口 `effective_of`；
//! * [`water_check`] —— 水位检查（M2-14）：扫描 / 判定 / 建待办 / 自动收口 / 定时注册；
//! * [`handler`] —— 接口层：4 条路由 + 参数校验 + 存在性校验 + 409/400 人话；
//! * 本文件 —— 模块地图与口径总述。
//!
//! 真库用例见 [`policy_db_tests`] 与 [`water_check_db_tests`]（`ERP_DB_URL` 开关，
//! 与其它 ERP 用例共用同一个串行锁）；**边界**用例（阈值刀口 / 4 位小数 / 明细上限 /
//! 重提醒整点 / 分页极值）见 `water_check::boundary_tests`（M2-16，纯逻辑不碰库）。

pub mod dto;
pub mod handler;
pub mod model;
pub mod water_check;

/// 水位设置的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod policy_db_tests;

/// 水位检查的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod water_check_db_tests;

pub use dto::{
    BatchPolicyPlan, BatchSavePolicyReq, BatchSavePolicyRes, EffectiveMin, MAX_BATCH_CELLS, MinSource, PolicyFilter,
    PolicyLevels, PolicyRes, PolicyWrite, STATUS_DISABLED, STATUS_ENABLED, SavePolicyReq, WaterCheckItem,
    WaterCheckReq, WaterCheckRes, checked_write, max_qty_allowed, min_qty_allowed, resolve_max_qty, resolve_min_qty,
};
pub use handler::{
    PERM_CHECK, PERM_DELETE, PERM_LIST, PERM_SAVE, STOCK_POLICY_PERMISSIONS, check_water_of, delete_policy_of,
    page_policy_of, save_policy_batch_of, save_policy_of,
};
pub use model::{
    EffectiveLevels, POLICY_TARGET, PolicyRow, effective_of, policy_find_by_id, policy_find_by_key, policy_page,
};
pub use water_check::{
    CRON as WATER_CHECK_CRON, DOC_TYPE as WATER_TODO_DOC_TYPE, TASK_NAME as WATER_CHECK_TASK_NAME, run_check,
};
