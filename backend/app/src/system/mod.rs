//! 系统管理模块（业务容器）。
//!
//! 承载账号与权限域的业务能力。每个子模块统一遵循
//! `mod.rs + handler.rs + dto.rs + model.rs` 结构，**不设 service 层**：
//! 业务编排写在 `handler.rs`，数据访问与领域行为写在 `model.rs`。
//!
//! | 子模块 | 职责 | 交付阶段 |
//! |--------|------|----------|
//! | [`auth`] | 登录、登出、令牌刷新、当前用户信息 | P2 |
//! | [`user`] | 用户管理（含角色分配、密码重置） | P2 |
//! | [`role`] | 角色管理与菜单授权 | P2 |
//! | [`menu`] | 菜单与权限点维护（树形结构） | P2 |
//! | [`dept`] | 部门机构（树形结构） | P2 |
//! | [`preference`] | 用户偏好体系（通用 KV + 字典元数据） | 批次 D |
//! | [`badge`] | 菜单徽标（公告未读 / 通知未读 / 待我审批 / 待我处理） | 批次 E |
//!
//! P1 阶段各子模块只交付强类型模型（`model.rs`，后缀 `*Model`）：结构体、字段
//! 与枚举先落定，接口层（`handler.rs` / `dto.rs`）随 P2 补齐。
//!
//! # 权限控制
//!
//! 管理端接口通过 `#[nik::permission]` 声明所需权限码（格式 `{模块}:{资源}:{动作}`），
//! 由框架的权限校验层在处理请求前拦截；权限码明细由 `menu` 模块的表数据驱动。

pub mod auth;
pub mod badge;
pub mod dept;
pub mod menu;
pub mod preference;
pub mod role;
pub mod setup;
pub mod user;

/// 测试C 共用夹具（**仅测试编译**，不产生任何生产符号）。
#[cfg(test)]
pub(crate) mod tests_support;

/// 测试C：越权用例集（O01–O14，源码注解全量扫描 + 与中间件同一判定谓词）。
#[cfg(test)]
mod authz_tests;

/// 测试C：会话/令牌版本回归防线（S01–S08，FIX-05 / BUG-SYS-04）。
#[cfg(test)]
mod session_tests;
