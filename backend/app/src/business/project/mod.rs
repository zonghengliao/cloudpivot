//! 项目分组（批次 C+ · T7c）：把 T1 建好但一直闲置的 `biz_project` 接上接口与页面。
//!
//! 依据：卡面 t_b9f70dab（「项目分组：CRUD + 统计 + 任务归入」）；
//!       `docs/批次C+-工作任务管理-核心设计.md` §十二（`biz_task.project_id` 是可选分组）。
//!
//! # 一句话定义
//!
//! **项目只是任务的逻辑分组，不是权限边界。** 项目负责人 / 项目成员都**不能**因此
//! 看到项目下的其它任务；任务可见性永远只走 T1 的 `数据范围 ∪ 参与关系`。
//!
//! # 本模块的 7 个接口（+ 3 个成员接口）
//!
//! 见 [`handler`] 的模块文档表格。
//!
//! # 三条不变式
//!
//! 1. **项目下任务列表只有一条实现路径**：`handler::tasks` 直接调
//!    [`crate::business::task::model::page_visible`]，本模块**不写**任何
//!    「`WHERE project_id = ?` 直接查 `biz_task`」的 SQL。
//! 2. **统计按 T1 裁剪**：[`model::stats_for_projects`] 注入与任务列表同源的可见性片段，
//!    所以「任务数 / 完成率」只算当前用户看得见的任务 —— 项目成员看不到「有个你看不见的任务」。
//! 3. **删项目不删任务**：`handler::delete` 在一个事务里软删项目 + `project_id` 置 NULL +
//!    删成员行；回执带 `unbound_tasks` 数量，验收脚本拿它和库里的行数对账。
//!
//! # 与其它模块的边界
//!
//! - **任务**：本模块只读任务（统计）与写一个字段（`project_id`）；状态机、进度、
//!   通知、变更日志一概不碰 —— 归入项目不是状态变更，不该触发通知。
//! - **数据权限**：项目用本地锚点 [`model::PROJECT_TARGET`]（不登记 `SCOPED_TABLES`，
//!   理由同 `business/task/model.rs::TASK_TARGET`）。
//! - **成员表**：`biz_project_member` 是 T1 漏建、本卡在 `V0115` 补建的表；
//!   成员只影响「项目本身可见」，不影响任务可见。

pub mod dto;
pub mod handler;
pub mod model;
