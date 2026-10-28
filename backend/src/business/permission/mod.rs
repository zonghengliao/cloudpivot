//! 数据权限（批次 B · B4）：范围配置 API + 部门树裁剪 + 自检接口 + 权限种子。
//!
//! # 一句话说清
//!
//! B2 把「数据权限」做成了基座（解析 `ScopedPrincipal`、生成 SQL 片段、缓存与失效）；
//! 本模块是它的**配置面与自检面**：谁能配、配什么、配完生效成什么样、哪里还没接上。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`dto`] | 请求 / 响应 DTO（ID 一律 `String`，标签随响应下发） |
//! | [`scope_model`] | 库侧读写（`sys_role` / `sys_role_dept` / `sys_dept`）+ 纯逻辑与单测 |
//! | [`scope_handler`] | 5 个 HTTP 接口（查 / 设范围、部门树、自检、未接入表） |
//!
//! # 接口一览（前缀 `/api` 由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET | `/business/permission/scope/role/{role_id}` | `business:permission:scope:read` |
//! | PUT | `/business/permission/scope/role/{role_id}` | `business:permission:scope:write` |
//! | GET | `/business/permission/scope/dept-tree` | `business:permission:scope:read` |
//! | GET | `/business/permission/scope/self-check` | `business:permission:scope:self-check` |
//! | GET | `/business/permission/scope/uncovered-tables` | `business:permission:scope:self-check` |
//!
//! 三个权限码与 `V0065__seed_batchb_permissions` 的 `sys_menu.auth_code` **逐字一致**
//! （不一致 = 接口 403，这是批次 A 踩过的坑，见 `V0045` 头注释）。
//!
//! # 与 B6 / B7 的边界（本卡不碰）
//!
//! - `sys_field_mask`（列级脱敏）= B6；
//! - `sys_role_mutex`（角色互斥）= B7；
//! - `system/` 侧的读路径接入（`page_users` / 部门树 / 启动自检接线）= B3；
//! - 其余登记表的接入 = B5。
//!
//! 本模块只提供「配置 + 自检」，**不接管任何业务查询**——那会把基座变回散点注入。

pub mod dto;
pub mod scope_handler;
pub mod scope_model;

pub use scope_model::{table_coverage, uncovered_tables};
