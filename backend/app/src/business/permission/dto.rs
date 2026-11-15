//! 数据权限配置 / 自检接口的请求与响应 DTO（批次 B · B4）。
//!
//! # 命名与类型约定
//!
//! - ID 一律用 `String`：本项目 `uuid::Uuid` 没开 serde feature（`business/shared/principal.rs`
//!   的 `CachedScope` 同样用 `String` 承载），DTO 里出现 `Uuid` 会直接编译不过；
//! - 字段名与前端契约保持一致（`data_scope` / `custom_dept_ids` / `where_sql`），
//!   前端不硬编码中文标签——标签随响应下发（`data_scope_label`），改文案不必改前端；
//! - 只读展示字段（`where_sql` 原文、`binds` 个数）**不包含参数值**：自检接口是运维
//!   排查「为什么他看不到」的窗口，不是枚举他人可见范围的探测工具（架构 §2.9 末注）。

use serde::{Deserialize, Serialize};

/// 一个角色的数据范围配置（`GET /business/permission/scope/role/{role_id}`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleScopeRes {
    /// 角色 ID。
    pub role_id: String,
    /// 角色编码（`SUPER_ADMIN` 等）。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 角色状态（0 停用 / 1 正常）。
    pub status: i16,
    /// 数据范围（1..5，与 `sys_role.data_scope` 一致）。
    pub data_scope: i16,
    /// 数据范围的稳定编码（`ALL` / `DEPT` / `DEPT_AND_CHILD` / `SELF` / `CUSTOM`）。
    pub data_scope_code: String,
    /// 数据范围的中文标签（前端直接展示，不硬编码）。
    pub data_scope_label: String,
    /// 自定义部门 ID 列表（仅 `data_scope=5` 有意义；其它范围也回显库里的存量配置）。
    pub custom_dept_ids: Vec<String>,
    /// 自定义部门明细（ID + 编码 + 名称），`custom_dept_ids` 的展示形态。
    pub custom_depts: Vec<DeptBrief>,
    /// 该角色当前持有者数量（改范围会让这些人的缓存失效，前端可提示影响面）。
    pub holder_count: i64,
}

/// 部门的精简信息（点名 / 穿梭框回显）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeptBrief {
    /// 部门 ID。
    pub id: String,
    /// 部门编码。
    pub dept_code: String,
    /// 部门名称。
    pub dept_name: String,
}

/// 设置数据范围的请求体（`PUT /business/permission/scope/role/{role_id}`）。
///
/// # `custom_dept_ids` 的三态语义（与架构 §2.9 一致）
///
/// - `null` / 字段缺省：**不动**已有的自定义部门集合（切到 2/3/4 时保留配置，便于切回）；
/// - `[]`：清空集合；
/// - `["…"]`：覆盖为给定集合，且只有 `data_scope=5` 时才接受非空集合。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SetRoleScopeReq {
    /// 数据范围（1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门集合）。
    pub data_scope: i16,
    /// 自定义部门 ID 列表（仅 `data_scope=5` 生效；见上方三态语义）。
    #[serde(default)]
    pub custom_dept_ids: Option<Vec<String>>,
}

/// 设置数据范围的结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetRoleScopeRes {
    /// 角色 ID。
    pub role_id: String,
    /// 生效后的数据范围。
    pub data_scope: i16,
    /// 生效后的数据范围编码。
    pub data_scope_code: String,
    /// 生效后的数据范围标签。
    pub data_scope_label: String,
    /// 生效后的自定义部门 ID 列表。
    pub custom_dept_ids: Vec<String>,
    /// 生效后的自定义部门明细。
    pub custom_depts: Vec<DeptBrief>,
    /// 本次被清除数据范围缓存的用户数（`sys_user_role` 反查结果）。
    pub invalidated_users: i64,
}

/// 自定义部门树节点（`GET /business/permission/scope/dept-tree`）。
///
/// 只含部门树需要的字段：**不含**负责人、电话、邮箱——部门树的用途是「选择范围」，
/// 多带一个字段就是多泄露一份组织信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeptTreeNode {
    /// 部门 ID。
    pub id: String,
    /// 上级部门 ID（裁剪后父节点不可见时为 `None`，即提升为根，见 `scope_model::crop_to_visible`）。
    pub parent_id: Option<String>,
    /// 部门编码。
    pub dept_code: String,
    /// 部门名称。
    pub dept_name: String,
    /// 状态（0 停用 / 1 正常）。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 子部门。
    pub children: Vec<DeptTreeNode>,
}

/// 部门树响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeptTreeRes {
    /// 本次返回的节点总数（**已按操作者范围裁剪**，用于前后端对账与验收对比）。
    pub total: usize,
    /// 根节点个数。
    pub root_count: usize,
    /// 是否按操作者范围裁剪过（`false` = 操作者是超管或范围=全部，返回的是全量树）。
    pub scope_limited: bool,
    /// 裁剪后保留下来的部门 ID（便于前端标注「不可选」的相邻部门）。
    pub visible_dept_ids: Vec<String>,
    /// 树本身。
    pub tree: Vec<DeptTreeNode>,
}

/// 数据范围的摘要（自检接口的 `scope` 字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeSummary {
    /// 数据范围下标（1..5）。
    pub data_scope: i16,
    /// 稳定编码。
    pub code: String,
    /// 中文标签。
    pub label: String,
}

/// 一个来源角色（自检接口的 `roles` 字段：解释「你的范围是哪个角色给的」）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRole {
    /// 角色 ID。
    pub role_id: String,
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 该角色配置的数据范围。
    pub data_scope: i16,
    /// 该角色数据范围的稳定编码。
    pub data_scope_code: String,
}

/// 一张登记表上最终会注入的片段（自检接口的 `tables` 字段）。
///
/// `where_sql` 只给片段原文、`binds` 只给参数个数——**不给参数值**（架构 §2.9 末注）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableFilter {
    /// 表名。
    pub table: String,
    /// 主体表别名（片段里用的别名，运维比对 SQL 时用）。
    pub alias: String,
    /// 片段原文（以 ` AND ` 开头）。
    pub where_sql: String,
    /// 绑定参数个数。
    pub binds: usize,
    /// 该片段等价于「不过滤」。
    pub is_noop: bool,
    /// 是否发生了安全侧降级（表缺部门列 / 部门集合为空等）。
    pub degraded: bool,
}

/// 数据范围缓存的埋点快照（自检接口的 `cache` 字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheInfo {
    /// 命中次数（进程内累计）。
    pub hits: u64,
    /// 未命中次数。
    pub misses: u64,
    /// 查库次数。
    pub db_loads: u64,
    /// 缓存不可用（回退查库）次数。
    pub cache_unavailable: u64,
    /// 当前 epoch（部门树变更时递增）。
    pub epoch: u64,
    /// 缓存 TTL（秒）。
    pub ttl_secs: u64,
}

/// 当前用户的数据权限自检结果（`GET /business/permission/scope/self-check`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfCheckRes {
    /// 用户 ID。
    pub user_id: String,
    /// 登录账号。
    pub username: String,
    /// 姓名（`real_name`）。
    pub real_name: String,
    /// 是否超级管理员（按库侧角色编码判定，不是令牌快照）。
    pub is_super_admin: bool,
    /// 用户所属部门 ID。
    pub own_dept_id: Option<String>,
    /// 用户所属部门名称。
    pub own_dept_name: Option<String>,
    /// 生效范围摘要。
    pub scope: ScopeSummary,
    /// 可见部门数量。
    pub dept_count: usize,
    /// 可见部门 ID 列表。
    pub dept_ids: Vec<String>,
    /// 可见部门明细。
    pub depts: Vec<DeptBrief>,
    /// 是否额外可见「自己的行」（多角色并集里出现过「仅本人」）。
    pub self_only: bool,
    /// 来源角色（生效范围的解释）。
    pub roles: Vec<SourceRole>,
    /// 各登记表最终注入的片段。
    pub tables: Vec<TableFilter>,
    /// 缓存埋点。
    pub cache: CacheInfo,
}

/// 一张登记表的接入状态（`GET /business/permission/scope/uncovered-tables`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableCoverage {
    /// 表名。
    pub table: String,
    /// 部门列（`None` = 该表没有部门列）。
    pub dept_col: Option<String>,
    /// 归属列（`None` = 该表没有归属人）。
    pub owner_col: Option<String>,
    /// 是否已处理（= `status != "uncovered"`）。保留给老前端与监控脚本（向后兼容）。
    pub wired: bool,
    /// 接入状态：`wired`（走 `scope::paginate` / 不分页的 `filter_for`）
    /// / `exempted`（`scope::paginate_unscoped` + `SCOPE-EXEMPT`）
    /// / `uncovered`（未接入，必须为 0 张）。
    pub status: String,
    /// 该表读取路径是否**不分页**（部门树）：`true` 时不存在 `{FILTER}` 位置契约，
    /// 只有 `scope::filter_for` 注入。
    pub no_paginate: bool,
    /// 判定依据 / `SCOPE-EXEMPT` 理由（**恒非空**：每条豁免都必须站得住）。
    pub reason: String,
}

/// 未接入登记表清单的响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UncoveredTablesRes {
    /// 登记表总数（`shared::scope::SCOPED_TABLES` 的长度）。
    pub registered: usize,
    /// 已接入数量（`status = "wired"`）。
    pub wired: usize,
    /// 显式豁免数量（`status = "exempted"`：`scope::paginate_unscoped` + `SCOPE-EXEMPT`）。
    pub exempted: usize,
    /// 未接入数量（**上线口径必须为 0**）。
    pub uncovered: usize,
    /// 未接入的表名（**运维排查的第一入口**：这些表上的查询还没有数据权限）。
    pub uncovered_tables: Vec<String>,
    /// 显式豁免的表名（附理由见 `tables[].reason`）。
    pub exempted_tables: Vec<String>,
    /// 全量明细（含已接入 / 已豁免的，每行都带 `status` + `reason`）。
    pub tables: Vec<TableCoverage>,
}
