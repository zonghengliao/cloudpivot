//! 任务批量操作（D7）：批量改状态 / 批量指派。
//!
//! # 做法
//!
//! 逐条调用任务模块**已有的** HTTP 入口（[`handler::change_status`] /
//! [`handler::assign`]）本体：可见性、角色判定（负责人 / 创建人 / 分派人 / 管理员）、
//! 子任务汇总拦截、状态联动（`DOING → max(进度,10)`）、通知与协作人日志全都在那里面，
//! 批量层重写一遍必然漏一条。**一条任务一个事务**，一条失败不影响其它条。
//!
//! # 为什么状态码沿用单条的 403 / 404
//!
//! 批量里「看不见的任务」和「看得见但没权限」的处理必须与单条一致，否则用户会遇到
//! 「单条点不了、批量却能改」的越权。所以这里**不做任何降级**，失败原因原样回传。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只按任务主键点名 biz_task（任务表；可见性由 model::find_detail
//! 的 scoped 查询 + actor_role 逐条判，批量层不放行任何范围）。
//! ```

use crate::utils;
use axum::extract::Path;
use axum::{Extension, Json};
use nik::prelude::*;

use crate::business::shared::batch::{self, BatchResult};
use crate::business::task::dto::{PERM_ASSIGN, TaskAssignReq, TaskStatusChangeReq};
use crate::business::task::handler::{assign, change_status};

/// 批量改状态的权限码（与单条 `PATCH /business/task/{id}/status` 一致）。
pub const PERM_STATUS: &str = "business:task:status";

/// 批量改状态请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchTaskStatusReq {
    /// 勾选的任务 id。
    pub ids: Vec<String>,
    /// 目标状态（`TODO` / `DOING` / `REVIEW` / `DONE` / `CANCELLED`）。
    pub status: Option<String>,
    /// 原因（打回场景必填，单条接口判）。
    pub reason: Option<String>,
}

/// 批量指派请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchTaskAssignReq {
    /// 勾选的任务 id。
    pub ids: Vec<String>,
    /// 新负责人（`sys_user.id`）。
    pub owner_id: Option<String>,
    /// 转派原因（≤ 500 字）。
    pub reason: Option<String>,
}

/// 批量改状态（单测直接调这一层）。
///
/// # 异常情况
///
/// - `403`：缺 `business:task:status`；
/// - `400`：`ids` 为空 / 超上限 / 非法 UUID / `status` 为空；
/// - 逐条失败（无权限 / 有子任务 / 已取消冻结）进 `failed`，其余继续。
pub async fn batch_task_status(
    claims: &TokenClaims,
    req: &BatchTaskStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_STATUS)?;
    let ids = batch::parse_ids(&req.ids)?;
    let to = req
        .status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ApiError::bad_request("status 不能为空（TODO / DOING / REVIEW / DONE / CANCELLED）"))?
        .to_string();
    let labels = batch::labels_of("biz_task", "task_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询任务编号"))?;
    let claims = claims.clone();
    let reason = req.reason.clone();
    Ok(batch::run(&ids, &labels, |id| {
        let claims = claims.clone();
        let body = TaskStatusChangeReq {
            to: to.clone(),
            reason: reason.clone(),
        };
        async move {
            change_status(Extension(AuthClaims(claims)), Path(id.to_string()), Json(body))
                .await
                .map(|_| ())
        }
    })
    .await)
}

/// 批量指派（单测直接调这一层）。
pub async fn batch_task_assign(
    claims: &TokenClaims,
    req: &BatchTaskAssignReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_ASSIGN)?;
    let ids = batch::parse_ids(&req.ids)?;
    let owner_id = req
        .owner_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ApiError::bad_request("owner_id 不能为空：批量指派要有新负责人"))?
        .to_string();
    let labels = batch::labels_of("biz_task", "task_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询任务编号"))?;
    let claims = claims.clone();
    let reason = req.reason.clone();
    Ok(batch::run(&ids, &labels, |id| {
        let claims = claims.clone();
        let body = TaskAssignReq {
            owner_id: owner_id.clone(),
            reason: reason.clone(),
        };
        async move {
            assign(Extension(AuthClaims(claims)), Path(id.to_string()), Json(body))
                .await
                .map(|_| ())
        }
    })
    .await)
}

/// 批量改状态。
#[nik::permission(
    path = "/business/task/batch-status",
    method = "POST",
    permission = "business:task:status"
)]
#[nik::post("/business/task/batch-status")]
pub async fn task_batch_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchTaskStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_task_status(&claims, &req).await?))
}

/// 批量指派。
#[nik::permission(
    path = "/business/task/batch-assign",
    method = "POST",
    permission = "business:task:assign"
)]
#[nik::post("/business/task/batch-assign")]
pub async fn task_batch_assign(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchTaskAssignReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_task_assign(&claims, &req).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "018f4b2c-1111-7222-8333-444455550001".to_string(),
            username: Some("d7-task".to_string()),
            roles: vec!["TEST".to_string()],
            permissions: permissions.iter().map(|item| (*item).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn batch_task_status_validates_permission_ids_and_target() {
        let id = "018f4b2c-1111-7222-8333-444455550002".to_string();
        let forbidden = batch_task_status(
            &claims_with(&["business:task:read"]),
            &BatchTaskStatusReq {
                ids: vec![id.clone()],
                status: Some("DONE".to_string()),
                reason: None,
            },
        )
        .await
        .expect_err("没有 status 码必须 403");
        assert_eq!(forbidden.status, 403);

        let empty = batch_task_status(
            &claims_with(&[PERM_STATUS]),
            &BatchTaskStatusReq {
                ids: Vec::new(),
                status: Some("DONE".to_string()),
                reason: None,
            },
        )
        .await
        .expect_err("空 ids 必须 400");
        assert_eq!(empty.status, 400);

        let missing_target = batch_task_status(
            &claims_with(&[PERM_STATUS]),
            &BatchTaskStatusReq {
                ids: vec![id],
                status: None,
                reason: None,
            },
        )
        .await
        .expect_err("没有目标状态必须 400");
        assert_eq!(missing_target.status, 400);
        assert!(missing_target.msg.contains("status"));
    }

    #[tokio::test]
    async fn batch_task_assign_requires_owner() {
        let error = batch_task_assign(
            &claims_with(&[PERM_ASSIGN]),
            &BatchTaskAssignReq {
                ids: vec!["018f4b2c-1111-7222-8333-444455550003".to_string()],
                owner_id: None,
                reason: None,
            },
        )
        .await
        .expect_err("没有新负责人必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("owner_id"));
    }
}
