//! 审批流程定义 · 数据访问层（批次 D · D1）。
//!
//! 依据：`docs/批次D-工作流审批-技术架构.md` §3.1（三个表示）、§3.6（版本管理）、§6.1（DDL）。
//!
//! # 三个表示（L1 / L2 / L3）在本模块的落点
//!
//! | 层 | 存哪 | 本模块的函数 |
//! |----|------|--------------|
//! | L1 编辑期行 | `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | [`load_graph`] / [`replace_graph`] |
//! | L2 发布快照 | `biz_flow_def_version.graph_snapshot` | [`apply_publish`]（写）+ [`load_snapshot`]（读） |
//! | L3 运行期内存图 | 进程内 LRU（D2 才建） | 不在本卡范围 |
//!
//! # 三条不变式
//!
//! 1. **PUBLISHED 版本的行走不了写路径**：[`replace_graph`] 的调用方（handler）先查
//!    `version_status`，只有 `DRAFT` 才允许覆盖；库层的 `ck_biz_flow_def_version_published`
//!    再兜一道（PUBLISHED 必须有快照 + hash + 发布时间）。
//! 2. **发布只改 `def.current_version_id` + 版本行状态，不动任何已有版本的图**
//!    （架构 D2：在跑实例绑的是自己的 `version_id`）。
//! 3. **`is_current` 与 `def.current_version_id` 同事务维护**：`uk_biz_flow_def_version_current`
//!    是库级「每个 def 至多一个当前版本」的部分唯一索引，两者不一致会立刻被它拦下。
//!
//! # 闸②（`scripts/check.sh`）与数据权限
//!
//! `biz_flow_*` 已登记进闸②清单，本文件在文件头写了 `// SCOPE-EXEMPT:` 说明：
//! **流程定义是全局配置，不参与数据范围裁剪**。理由：流程定义不是「谁的业务数据」，
//! 它是「全平台共用的规则」；若按部门范围过滤，A 部门的人会看不到自己的单据要走哪个流程，
//! 而编辑权限已经由 `business:flow:update` / `publish` 这类权限码隔离。
//! 这与 `biz_task_status_def`（全局字典）的豁免口径一致，与 `biz_task`（有 owner/dept）相反。

// ==================== 部件（TD-2 拆分，facade 重导出） ====================
//
// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。

pub use super::flow_model_base::*;
pub use super::flow_model_cancel::*;
pub use super::flow_model_def::*;
pub use super::flow_model_graph::*;
pub use super::flow_model_guard::*;
pub use super::flow_model_inbox::*;
pub use super::flow_model_instance::*;
pub use super::flow_model_node::*;
pub use super::flow_model_progress::*;
pub use super::flow_model_stats::*;

// ==================== 测试（#[path] 挂到各测试部件） ====================
#[cfg(test)]
#[path = "flow_model_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "flow_model_delete_tests.rs"]
mod t12_delete_flow_tests;
