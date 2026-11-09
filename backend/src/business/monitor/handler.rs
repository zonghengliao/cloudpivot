//! 服务器状态接口处理器。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/api/business/monitor/server-status` | `business:monitor:server-status` | 页面数据（CPU / 内存 / 磁盘 / DB / Redis / 主机 / 进程 / 业务量 / Agent 探测） |
//! | GET | `/api/system/server-status/diagnostic` | `system:server-status:view` | 下载诊断包（zip，仅超管，密码已打码，动作写 `operation_logs`） |
//!
//! 两个接口的宏路径都**不含** `/api` 前缀（由 `config.toml` 的 `[router] prefix` 注入）。
//! 权限码只授予 `SUPER_ADMIN`，非超管调用会被 `#[nik::permission]` 校验层直接拒绝（403）。
//!
//! # 为什么诊断包挂在 `/system/...` 下
//!
//! 页面路由是 `/system/server-status`（系统管理 → 服务器状态），诊断包是**这个页面的
//! 附件下载**，路径跟着页面走；而页面数据复用既有的 `business/monitor` 接口
//! （首页那块服务器状态卡一直用它，改路径会连带改前端已有调用）。

use axum::Extension;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::monitor::diagnostic::{self, OP_DIAGNOSTIC};
use crate::business::monitor::dto::{self, ServerStatusRes};
use crate::business::monitor::model;
use crate::utils;

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
#[nik::permission(
    path = "/business/monitor/server-status",
    method = "GET",
    permission = "business:monitor:server-status"
)]
#[nik::get("/business/monitor/server-status")]
pub async fn server_status() -> ResResult<ServerStatusRes> {
    Ok(Res::ok(model::collect().await))
}

/// 下载诊断包（zip）。
///
/// # 参数
///
/// - `claims`：当前用户声明（超管判定，**不**取自请求参数）；
/// - `headers`：请求头（只用于审计：客户端 IP / UA / 请求 ID）。
///
/// # 返回值
///
/// `application/zip` 裸响应，`Content-Disposition` 带时间戳文件名，内含
/// `info.json` / `health.json` / `server-status.json` / `metrics.txt` /
/// `config-redacted.txt` / `migrations.txt` / `logs-tail.txt`（见
/// [`crate::business::monitor::diagnostic`]）。
///
/// # 异常情况
///
/// - 403：非超管（权限码只授超管 + [`dto::ensure_super_admin`] 第二层判定）；
/// - 500：zip 组装失败（此时页面数据仍可看，不影响监控本身）。
#[nik::permission(
    path = "/system/server-status/diagnostic",
    method = "GET",
    permission = "system:server-status:view"
)]
#[nik::get("/system/server-status/diagnostic")]
pub async fn diagnostic(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    dto::ensure_super_admin(&claims, "下载")?;
    let bundle = diagnostic::build().await?;

    // 下载留痕：审计要能回答「谁在什么时候下过诊断包」。
    // 写日志失败**不阻断下载**（用户要的是包），但必须留下告警。
    if let Err(error) = write_diagnostic_log(&claims, &headers, &bundle.file_name, bundle.bytes.len()).await {
        tracing::warn!(error = %error, "写诊断包下载审计失败（不影响本次下载）");
    }

    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", bundle.file_name))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"cloudpivot-diagnostic.zip\""));

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/zip")),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate"),
            ),
        ],
        bundle.bytes,
    )
        .into_response())
}

/// 写一条「下载诊断包」审计（`operation_logs`）。
///
/// # 为什么直写而不是投递日志总线
///
/// 与附件上传 / 删除留痕同一条理由：总线是批量异步的，未初始化进程里会丢事件；
/// 而「谁下载了含配置与日志的包」正是审计要回答的问题。
async fn write_diagnostic_log(
    claims: &TokenClaims,
    headers: &HeaderMap,
    file_name: &str,
    size_bytes: usize,
) -> DbResult<()> {
    let user_id = claims.sub.parse::<Uuid>().ok();
    let client_ip = utils::client_ip(headers, None);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let request_id = utils::request_id(headers);

    db().execute_with_params(
        "INSERT INTO operation_logs \
             (id, request_id, user_id, username, operation, target_type, target_id, detail, \
              ip, user_agent, duration_ms, request_path, request_method, status_code, created_at) \
         VALUES (?::uuid, ?, ?::uuid, ?, ?, 'system_server_status', ?, ?::jsonb, \
                 ?::text::inet, ?, NULL, ?, 'GET', 200, now())",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(request_id),
            user_id.as_ref().map(Value::from).unwrap_or(Value::Null),
            Value::Text(claims.username.clone().unwrap_or_default()),
            Value::Text(OP_DIAGNOSTIC.to_string()),
            Value::Text(file_name.to_string()),
            Value::Json(
                json!({
                    "file_name": file_name,
                    "size_bytes": size_bytes,
                    // 包内清单是固定契约（前端「下载诊断包」的说明文案与之一致）
                    "members": [
                        "info.json", "health.json", "server-status.json", "metrics.txt",
                        "config-redacted.txt", "migrations.txt", "logs-tail.txt"
                    ],
                    "redacted": true,
                })
                .to_string(),
            ),
            client_ip.map(|ip| Value::Text(ip.to_string())).unwrap_or(Value::Null),
            user_agent.map(Value::Text).unwrap_or(Value::Null),
            Value::Text("/api/system/server-status/diagnostic".to_string()),
        ],
    )
    .await
    .map(|_affected| ())
}

#[cfg(test)]
mod tests {
    /// 诊断接口的权限码必须与菜单种子里的 `auth_code` 逐字一致。
    ///
    /// 这条断言防的是「改了注解没改种子」导致的**功能上线即死锁**：
    /// 注解要的码在 `sys_menu` 里没有 → 除超管外全员 403；反过来种子种了码、
    /// 注解写错 → 授权树里勾了也不生效。真库侧的同一约束见
    /// `system::menu::tests::n10`（扫源码注解 ↔ 菜单表）。
    ///
    /// 菜单为什么在 seed 而不是编号迁移：`sys_menu.parent_id` 指向「系统管理」目录，
    /// 而该目录由 `S__menu_data.sql` 建立（seed 在所有编号迁移**之后**才跑）。
    /// 编号迁移里插子菜单只能拿到空父 id（空库首启）→ 违反 `menu_invariants.sh` ②；
    /// 父节点固定 UUID 在迁移期也还不存在。故按项目既有约定放进 seed。
    #[test]
    fn diagnostic_permission_code_matches_seed() {
        const PERM: &str = "system:server-status:view";
        const MENU_ID: &str = "10000000-0000-0000-0000-000000000090";

        let menu_seed = include_str!("../../../migrations/seed/S__menu_data.sql");
        assert!(menu_seed.contains(PERM), "菜单种子必须种入权限码 {PERM}");
        assert!(menu_seed.contains(MENU_ID), "菜单种子必须包含固定 UUID {MENU_ID}");
        assert!(
            menu_seed.contains("/system/server-status"),
            "菜单种子必须种入页面路径 /system/server-status"
        );
        assert!(
            menu_seed.contains("system/server-status/index"),
            "菜单 component 必须是 system/server-status/index（前端页面路径）"
        );

        let role_seed = include_str!("../../../migrations/seed/S__role_data.sql");
        assert!(
            role_seed.contains(MENU_ID),
            "角色种子必须把 {MENU_ID} 授给 SUPER_ADMIN（否则权限树里看不到这一项）"
        );
    }
}
