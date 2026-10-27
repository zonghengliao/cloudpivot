//! 服务器状态接口处理器。
//!
//! 单个只读接口：`GET /api/business/monitor/server-status`（后端宏路径不含 `/api`，
//! 前缀由 `config.toml` 的 `[router] prefix` 注入）。
//!
//! 权限码 `business:monitor:server-status` 在迁移 `V0020` 中只授予 `SUPER_ADMIN`，
//! 因此非超管调用会被 `#[alun::permission]` 校验层直接拒绝（403），而不是仅靠前端隐藏。

use alun::prelude::*;

use crate::business::monitor::dto::ServerStatusRes;
use crate::business::monitor::model;

/// 服务器运行状态（CPU / 内存 / 磁盘 / 数据库 / Redis）。
///
/// # 返回值
///
/// 始终 `code = 0`：单项指标采集失败时对应字段降级为 `0` / `state = "down"`，
/// 由前端展示为 `-`，不因外部依赖抖动导致整卡报错。
///
/// # 异常情况
///
/// - 403：当前用户不具备 `business:monitor:server-status`（非超级管理员）。
#[alun::permission(
    path = "/business/monitor/server-status",
    method = "GET",
    permission = "business:monitor:server-status"
)]
#[alun::get("/business/monitor/server-status")]
pub async fn server_status() -> ResResult<ServerStatusRes> {
    Ok(Res::ok(model::collect().await))
}
