//! 菜单徽标（红点 + 数字）—— 侧边栏四类提醒的聚合读接口。
//!
//! # 一句话
//!
//! 老大原话：「公告与消息、审批流、工作任务是不是应该有提示有新的内容？比如在后面有个
//! 红色圆点，圆点里面显示新内容条数；打开目录以后，公告中心和消息中心也应该有新内容提示。」
//!
//! 本模块就是那个**数字**的来源：一个接口、四个桶，前端把它们映射到具体菜单，
//! 目录徽标由前端把子菜单的桶**去重**后相加。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`model`] | 四条 count SQL（并发执行）+ 响应结构 + 口径常量 |
//! | [`handler`] | `GET /system/badge/counts`（无权限码，任何登录用户查自己的） |
//!
//! # 为什么放在 `system/` 而不是 `business/`
//!
//! 徽标**不属于**任何单一业务域：它横跨公告（C2）、通知（A5）、审批（D2）、任务（T3-C+）
//! 四个模块。放进任一业务模块都会让另外三个模块「反向依赖」它。放在 `system/`
//! 与 `system/preference`（同样是「用户自己的东西」）同层，语义上归「用户侧的通用读接口」。
//!
//! 跨层调用（`system::badge` → `business::*::model`）在本仓有既有先例：
//! `system/auth/handler.rs`、`system/dept/handler.rs`、`system/role/handler.rs` 都直接
//! 依赖 `business::shared::*` 与 `business::changelog`。
//!
//! # 迁移
//!
//! **0**：四个桶全部落在既有表与既有索引上（见 [`model`] 的索引对照表），不新增表 / 列 / 索引。
//!
//! # 前端对应
//!
//! - `frontend/src/api/badge.ts`（路径 `/api/system/badge/counts`）
//! - `frontend/src/stores/badge.ts`（30 秒轮询 + 页面隐藏即停）
//! - `frontend/src/layouts/components/SideMenuItem.vue`（菜单徽标 / 目录累加）

pub mod handler;
pub mod model;
