//! 数据权限基座（批次 B · B2）。
//!
//! 依据 `docs/批次B-技术架构.md` §2（数据权限）与 §5.4（S1–S14 测试要求）。
//!
//! # 三个文件的分工
//!
//! | 文件 | 内容 | 谁用 |
//! |------|------|------|
//! | [`principal`] | [`principal::ScopedPrincipal`] + [`principal::resolve`]（一次往返解析 + Redis 缓存 + 埋点） | 所有需要知道「当前用户能看到哪些数据」的地方 |
//! | [`scope`] | [`scope::ScopeKind`] / [`scope::RowFilter`] / [`scope::ScopeTarget`] / [`scope::filter_for`] / [`scope::paginate`]（列表查询唯一入口） | 列表分页走 `paginate`；单条 / 导出 / 下拉走 `filter_for` |
//! | [`guard`] | `bump_epoch` / `invalidate_user_scope` / `preflight_permission` / `ensure_scope_grant_allowed` | 部门与授权写点（失效）、`bootstrap`（自检）、角色范围配置接口（上限校验） |
//!
//! # 本卡（B2）的边界
//!
//! **只做基座，不接入任何已有查询**。接入是 B3（`page_users`、部门树裁剪、4 个
//! `bump_epoch` 写点、`bootstrap` 自检）与 B5（日志 / 导入导出 / 公告 / 文件）的工作。
//! 因此本模块交付时 `scripts/check.sh` 的门禁命中数**应当保持**闸 ①=20 / 闸 ②=21
//! ——门禁数下降才是「接入发生了」的证据。
//!
//! # 用法（B3/B5 的接入形态）
//!
//! ```ignore
//! use crate::business::shared::{principal, scope};
//!
//! // 列表分页：唯一入口
//! let me = principal::resolve(&claims.user_id).await?;
//! let page = scope::paginate(
//!     scope::ScopeTarget::SYS_USER,          // 主体表锚点（只锚定主体表，不进 LEFT JOIN ON）
//!     "SELECT u.*, d.dept_name FROM sys_user u \
//!      LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
//!      WHERE u.deleted_at IS NULL",
//!     filter_params,                          // 业务过滤条件的参数（不含 scope 参数）
//!     Some("ORDER BY u.created_at DESC"),     // ORDER BY 单独传（count 会剥掉）
//!     page, page_size, &me,
//! ).await?;
//!
//! // 单条 / 导出：显式消费 RowFilter
//! let filter = scope::filter_for(&me, scope::ScopeTarget::SYS_EXPORT_TASK);
//! sql.push_str(filter.sql());
//! params.extend(filter.params().iter().cloned());
//! ```

pub mod guard;
pub mod principal;
pub mod scope;
