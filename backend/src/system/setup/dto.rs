//! 首次启动向导 · 请求 / 响应结构（FIX-1）。
//!
//! 三个接口的出入参都在这里：`check` / `assign` / `demo-users`。
//! 字段名就是**前端契约**（`frontend/src/api/system/setup-api.ts` 同形）。

use serde::{Deserialize, Serialize};

/// `GET /system/setup/check` 的响应。
#[derive(Debug, Clone, Serialize)]
pub struct SetupCheckRes {
    /// 关键角色是否全部就绪（`missing` 为空）。
    pub all_ready: bool,
    /// 演示账号种子开关是否打开（`NIK_SEED_DEMO_USERS`）。
    pub demo_users_enabled: bool,
    /// 演示账号口令从哪个环境变量取（只报变量名，**不回传口令**）。
    pub demo_password_env: String,
    /// 缺人的关键角色（`usable_member_count == 0`），向导逐个让用户选人。
    pub missing: Vec<super::model::KeyRoleStatus>,
    /// 全部关键角色现状（含已就绪的，供向导展示「哪些已配好」）。
    pub roles: Vec<super::model::KeyRoleStatus>,
}

/// `POST /system/setup/assign` 的请求：给某个角色指派（追加）成员。
#[derive(Debug, Clone, Deserialize)]
pub struct SetupAssignReq {
    /// 角色编码（如 `SALES_SUPERVISOR`）。跨环境稳定，前端优先用它。
    #[serde(default)]
    pub role_code: Option<String>,
    /// 角色 id（`role_code` 缺失时的备选；两者都给时以 `role_code` 为准）。
    #[serde(default)]
    pub role_id: Option<String>,
    /// 要加入该角色的用户 id 列表（**只增不减**）。
    #[serde(default)]
    pub user_ids: Vec<String>,
}

/// `POST /system/setup/assign` 的响应。
#[derive(Debug, Clone, Serialize)]
pub struct SetupAssignRes {
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 本次真正新加进角色的人。
    pub added: Vec<String>,
    /// 本来就在该角色里、本次跳过的人（幂等的证据）。
    pub skipped: Vec<String>,
    /// 指派完成后该角色的在职成员数。
    pub member_count: i64,
}

/// `POST /system/setup/demo-users` 的请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SetupDemoUsersReq {
    /// 是否强制要求开关打开（默认 `true`：开关没开就 409，不静默什么都不做）。
    #[serde(default)]
    pub force: bool,
}

/// `POST /system/setup/demo-users` 的响应。
#[derive(Debug, Clone, Serialize)]
pub struct SetupDemoUsersRes {
    /// 开关是否打开（`false` 时下面两个列表为空，且 HTTP 409）。
    pub enabled: bool,
    /// 新建的登录名。
    pub created: Vec<String>,
    /// 已存在、本次跳过的登录名。
    pub existed: Vec<String>,
    /// 演示账号统一口令**所在的变量名**（不回传口令本身）。
    pub password_env: String,
    /// 本次新建立的「用户-角色」绑定条数。
    pub bindings_added: usize,
    /// 本次补上部门的老账号数（`dept_id IS NULL → 演示部门`；全新库首启恒为 0）。
    pub dept_backfilled: usize,
}
