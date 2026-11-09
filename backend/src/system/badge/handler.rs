//! 菜单徽标的 HTTP 接口。
//!
//! # 接口一览（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/system/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/system/badge/counts` | **无** | 当前登录用户的四个徽标桶 |
//!
//! # 为什么没有权限码
//!
//! 徽标是「我自己的未读 / 待办」，任何登录用户都该看到 —— 与 `system/preference`
//! 同一个道理（见 `system/preference/handler.rs` 的注释）。挂上 `business:xxx:read`
//! 这类权限码会出现两种坏情况：
//!
//! 1. 只授了部分模块权限的用户（如只有任务权限），侧边栏其它目录永远没有红点；
//! 2. 新增权限点要同步种 `sys_menu`，否则整个接口 403、**所有**徽标一起消失。
//!
//! # 越权防线
//!
//! 只有一条：用户 ID 只从 `Extension(AuthClaims(claims))` 的 `sub` 取
//! （`utils::require_uuid` 校验形态），接口**没有** `{user_id}` 路径参数、
//! 也**不接受**任何请求体 —— 没有「查别人徽标」的入口。

use nik::prelude::*;

use crate::system::badge::model::{self, BadgeCounts};
use crate::utils;

/// 当前登录用户的四类徽标计数。
///
/// # 返回值
///
/// `{ "announcement_unread": 3, "notice_unread": 2, "flow_todo": 4, "task_todo": 2 }`
/// —— 四个桶恒存在（值为 0 时也返回 0，前端据此决定「不显示红点」而不是「字段缺失」）。
///
/// # 异常情况
///
/// - 400：令牌里的 `sub` 不是合法 UUID（认证层已保证，防御性保留）；
/// - 500：数据库异常（**不降级成全 0**，否则用户以为没有待办）。
///
/// # 日志
///
/// 不写业务日志：前端每 30 秒轮询一次，每次进页面都会打，写日志就是噪声。
#[nik::get("/system/badge/counts")]
pub async fn counts(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<BadgeCounts> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let counts = model::load(user_id)
        .await
        .map_err(|error| utils::db_error(error, "统计菜单徽标"))?;
    Ok(Res::ok(counts))
}
