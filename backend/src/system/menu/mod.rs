//! 菜单与权限点子模块。
//!
//! | 文件 | 内容 |
//! |------|------|
//! | [`model`] | [`model::SysMenuModel`]（`sys_menu`）、[`model::MenuType`]（菜单类型枚举） |
//!
//! 对外路径前缀 `/api/system/menu`。菜单以 `parent_id` 组织为树，`auth_code`
//! 承载权限码（`{模块}:{资源}:{动作}`）。

pub mod dto;
pub mod handler;
pub mod model;

/// 测试C：菜单模块用例集（N01–N12，真库 + 真实 handler + 三方一致性扫描）。
#[cfg(test)]
mod tests;

/// FIX-06：死权限码的**反向**不变量（N14–N18；菜单码 → 源码落地、路由 → 权限闸）。
#[cfg(test)]
mod permission_audit_tests;
