//! 工作任务管理 · 接口层（批次 C+ · T1）。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §七（可见性）、§十（两秒建任务）、
//! §十一（页面）、§十三 T1 行（6 个接口）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/page` | `business:task:read` | 任务分页（**可见性过滤** + 6 个筛选维度） |
//! | POST | `/business/task` | `business:task:create` | 创建（默认值：负责人=自己 / TODO / P2 / 本周五 / 需验收=关） |
//! | POST | `/business/task/{id}/copy` | `business:task:create` | **复制为新任务**（D9：标题加「（副本）」，不复制子任务/评论） |
//! | PUT | `/business/task/{id}` | `business:task:update` | 编辑（**仅负责人 / 创建人**） |
//! | DELETE | `/business/task/{id}` | `business:task:delete` | 软删除（负责人 / 创建人 / 超管） |
//! | GET | `/business/task/{id}` | `business:task:read` | 详情（子任务 + 协作人 + 附件） |
//! | GET | `/business/task/my` | `business:task:read` | 我的任务（逾期 / 今日 / 本周 / 以后 / 无期限） |
//!
//! ## T2 追加的 4 个（状态机 + 进度 + 子任务）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | PATCH | `/business/task/{id}/status` | `business:task:status` | 改状态（**白名单流转**，非法一律 4xx） |
//! | PATCH | `/business/task/{id}/progress` | `business:task:progress` | 改进度（有子任务 → **409**） |
//! | POST | `/business/task/{id}/subtask` | `business:task:subtask` | 建子任务（继承父任务的项目 / 优先级） |
//! | GET | `/business/task/{id}/subtasks` | `business:task:read` | 子任务列表（含各自进度 / 状态） |
//!
//! ## T3 追加的 4 个（分派 / 转派 / 协作人）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | PATCH | `/business/task/{id}/assign` | `business:task:read` | 分派 / 转派（行级矩阵 + `business:task:assign`） |
//! | POST | `/business/task/{id}/members` | `business:task:read` | 加协作人（≤50，幂等，强制通知） |
//! | DELETE | `/business/task/{id}/members/{user_id}` | `business:task:read` | 移除协作人（创建人/负责人 400） |
//! | GET | `/business/task/{id}/members` | `business:task:read` | 协作人列表（**带可见性**，看不见 404） |
//!
//! ## T4 追加的 4 个（依赖 / 甘特图）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/task/{id}/dependencies` | `business:task:read` | 前置 + 后继两组（**带可见性**） |
//! | POST | `/business/task/{id}/dependencies` | `business:task:read` | 加前置（自依赖 / 父子 / 跨项目 / 成环 一律 400） |
//! | DELETE | `/business/task/{id}/dependencies/{dep_id}` | `business:task:read` | 解除依赖（`dep_id` = 对端任务 id，双向匹配） |
//! | GET | `/business/task/gantt` | `business:task:read` | 甘特数据（时间窗 ≤ 180 天 + 任务 + 依赖边） |
//!
//! ### 依赖的三个「必须服务端做」的判断
//!
//! 1. **环检测**：加一条依赖后如果成环，必须 `400 + 63101` 并**点名环上每个任务**
//!    （红线：不许只回「有环」）。实现在事务内做（[`add_dependency`]），
//!    检测与插入之间用 `pg_advisory_xact_lock` 串行化 —— 否则两个并发请求各自
//!    「单独看都不成环」也会一起把圈闭上。
//! 2. **进 DOING 的前置校验**：`TODO → DOING` 时列未完成前置（`409 + 63102`），
//!    前置 `CANCELLED` 不算阻塞（[`crate::business::task::dep::is_blocking_status`]）。
//! 3. **前置完成 / 取消的通知**：`DONE` 强制通知后继负责人「可以开始了」，
//!    `CANCELLED` 尊重偏好提醒「前置取消了，你可能要调整计划」。
//!
//! ### 为什么中间件闸是 `business:task:read` 而不是 `business:task:assign`
//!
//! 分派权的真实口径是「**行级关系**」：创建人 / 当前负责人 / 分派人天然可派。
//! 这三类身份不是权限码能表达的——若把 `business:task:assign` 写进
//! `#[nik::permission]`，创建人（他往往没有这个码）会被中间件挡在门外，
//! 「创建人可以把活派出去」这条最基本的产品语义直接失效。
//!
//! 所以：**中间件只做入口闸**（能看任务的人才谈得上分派），真矩阵在 handler 里
//! （[`actor_role`] + [`may_assign`]）；`business:task:assign` / `business:task:member`
//! 两个权限码是**加项**——给管理员 / 「AI 团队主管」这类需要跨任务派活的角色用。
//! V0109 只把这两个码授给超管，其余角色要什么就勾什么（全员授予等于没有隔离：
//! 「无关用户分派 → 403」这条验收就不成立了）。
//!
//! # 三条贯穿全卡的硬约束
//!
//! 1. **读路径一律带可见性**：`/page`、`/{id}`、`/{id}` 的子任务列表都走
//!    [`crate::business::task::model::page_visible`] / `find_detail` / `list_children`。
//!    看不见的任务返回 **404**（不是 403）—— 403 会告诉调用者「这个 ID 存在」，
//!    等于把任务 ID 变成了可枚举的资源清单。
//! 2. **写路径一律先查再判**：编辑/删除先 `model::find_raw`（不做可见性过滤，但要判断
//!    「我是不是负责人/创建人」），再决定放行 —— 用可见性做写权限会让「协作人能改」。
//! 3. **状态与进度不在本卡**：`status` / `progress` / `done_at` / 取消原因 / 打回原因
//!    都是 T2 状态机的交付物，本卡的 PUT 不接受这些字段（见 `dto::TaskUpdateReq` 文档）。
//!
//! # 权限码 vs 数据范围（最容易被搞混的一处）
//!
//! 权限码（`business:task:*`）决定**能不能进这个功能**；数据范围（`sys_role.data_scope`）
//! 决定**能看到哪些任务**。V0103 把 4 个权限码授给了所有角色，所以「谁都进得来」；
//! 真正的隔离在 [`model::page_visible`] 的可见性谓词里（scope ∪ 参与关系）。
//! ## T13 追加的 1 个（父任务状态巡检 / 兜底对账）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/task/admin/repair-parent-status` | `business:task:update` | 扫「子任务全终态、父任务非 `REVIEW`」并修（`dry_run` 缺省只报不修；仅超管） |
//!
//! 为什么需要它：审批通过的 `REVIEW → DONE` 由 `flow::task_bridge::settle` 写，
//! 而 settle 跑在审批事务提交之后 ⇒ 级联只能开独立事务，极端失败会留下
//! 「子任务全终态、父任务停在 DOING」。本接口是那条链路的巡检兜底。

// ==================== 部件（TD-2 拆分，facade 重导出） ====================
//
// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。

// 公开 API 由 `pub use` 拼回；`common` 里的条目在拆分前就是 `pub(crate)`（同一文件内
// 被 `burndown` / `collab` 等兄弟模块 `use crate::business::task::handler::{...}` 引用），
// 因此这里用 `pub(crate) use` 保持**与拆分前一致**的可见性（`pub use` 会把 lint 拉响：
// 没有任何条目能导出到 `pub`）。`notice` 里的条目拆分前即私有，无需对外重导出。
pub use super::task_handler_ai::*;
pub use super::task_handler_assign::*;
pub use super::task_handler_attachment::*;
pub use super::task_handler_comment::*;
pub(crate) use super::task_handler_common::*;
pub use super::task_handler_crud::*;
pub use super::task_handler_dep::*;
pub use super::task_handler_flow::*;
pub use super::task_handler_hierarchy::*;
pub use super::task_handler_status::*;

// ==================== 测试（#[path] 挂到各测试部件） ====================
#[cfg(test)]
#[path = "task_handler_stats_tests.rs"]
mod stats_handler_tests;

#[cfg(test)]
#[path = "task_handler_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "task_handler_copy_db_tests.rs"]
mod copy_db_tests;
