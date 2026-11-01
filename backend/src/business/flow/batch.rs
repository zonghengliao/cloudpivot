//! 审批中心批量审批（D7）：勾一批待办 → 逐条同意 / 驳回 → 逐条报告。
//!
//! # 为什么直接调 HTTP handler
//!
//! 「同意」这条路上除了状态机还有**六个别业务回调**（任务 / 调拨 / 收货作废 / 收货过账 /
//! 采购审批 / 应付付款），任何一处漏掉都会留下「审批单结束了、单据永远卡在审批中」的孤儿。
//! 所以批量层**不重写这条路**，而是逐条调用 [`handler::approve_task`] / [`handler::reject_task`]
//! 本体：一条待办一个请求级事务，一条失败不影响其它条。
//!
//! # 与单条接口的同异
//!
//! | | 单条 | 批量 |
//! |---|---|---|
//! | 权限码 | `business:flow:approve` | 同（另在函数内再判一次） |
//! | 内控闸（自审自批） | 403 | 同，**逐条**判（谁的单子谁批不过去） |
//! | 事务 | 一个请求一个事务 | **一条待办一个事务** |
//! | 失败 | 整请求失败 | 该条进 `failed`，其余继续 |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只按待办主键点名 biz_flow_task（审批待办表，无 dept_id / 归属人列），
//! 「这条待办是不是你的」由 engine::approve/reject 逐条判（403 + 65028），批量层不越权放行。
//! ```

use crate::utils;
use axum::extract::Path;
use axum::{Extension, Json};
use nik::prelude::*;

use crate::business::flow::dto::{ApproveReq, RejectReq};
use crate::business::flow::handler::{approve_task, reject_task};
use crate::business::shared::batch::{self, BatchResult};

/// 批量审批的权限码（与单条 `/approve`、`/reject` 一致）。
pub const PERM_APPROVE: &str = "business:flow:approve";

/// 同意。
pub const DECISION_APPROVE: &str = "APPROVE";
/// 驳回。
pub const DECISION_REJECT: &str = "REJECT";

/// 批量审批请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchApproveReq {
    /// 勾选的待办 id。
    pub ids: Vec<String>,
    /// `APPROVE`（默认）/ `REJECT`。
    pub decision: Option<String>,
    /// 审批意见（同意可空；**驳回必填 ≥ 5 字**，由单条接口判）。
    pub comment: Option<String>,
    /// 驳回退回目标（`PREV` 默认 / `INITIATOR` / `NONE` / 节点 key）。
    pub return_to: Option<String>,
}

impl BatchApproveReq {
    /// 归一化决策（大小写不敏感；空 = 同意；其它值 400）。
    pub fn decision(&self) -> std::result::Result<&'static str, ApiError> {
        let raw = self
            .decision
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(DECISION_APPROVE)
            .to_ascii_uppercase();
        match raw.as_str() {
            DECISION_APPROVE => Ok(DECISION_APPROVE),
            DECISION_REJECT => Ok(DECISION_REJECT),
            other => Err(ApiError::bad_request(format!(
                "decision = `{other}` 不在允许的值里：APPROVE / REJECT"
            ))),
        }
    }
}

/// 待办的可读名称：`业务单号 / 节点`（业务单号为空时退回实例号）。
const LABEL_SQL: &str = "SELECT t.id::text AS id, \
     (COALESCE(i.biz_no, i.instance_no) || ' / ' || t.node_key)::text AS no \
     FROM biz_flow_task t \
     INNER JOIN biz_flow_instance i ON i.id = t.instance_id \
     WHERE t.id = ANY(?::text::uuid[])";

/// 批量同意 / 驳回（单测直接调这一层）。
///
/// # 异常情况
///
/// - `403`：缺 `business:flow:approve`；
/// - `400`：`ids` 为空 / 超上限 / 非法 UUID / `decision` 非法；
/// - 逐条失败（不是你待办 / 已结束 / 驳回意见太短）进 `failed`，其余继续。
pub async fn batch_approve(
    claims: &TokenClaims,
    req: &BatchApproveReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_APPROVE)?;
    let ids = batch::parse_ids(&req.ids)?;
    let decision = req.decision()?;
    let labels = batch::labels_from(LABEL_SQL, &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询待办名称"))?;
    let claims = claims.clone();
    let comment = req.comment.clone();
    let return_to = req.return_to.clone();
    Ok(batch::run(&ids, &labels, |id| {
        let claims = claims.clone();
        let comment = comment.clone();
        let return_to = return_to.clone();
        async move {
            if decision == DECISION_REJECT {
                let body = RejectReq {
                    // 驳回意见必填：空串交给单条接口判「至少 5 字」，报文与单条一致
                    comment: comment.unwrap_or_default(),
                    return_to,
                };
                reject_task(
                    Extension(AuthClaims(claims)),
                    Path(id.to_string()),
                    Json(body),
                )
                .await
                .map(|_| ())
            } else {
                let body = ApproveReq { comment };
                approve_task(
                    Extension(AuthClaims(claims)),
                    Path(id.to_string()),
                    Some(Json(body)),
                )
                .await
                .map(|_| ())
            }
        }
    })
    .await)
}

/// 批量同意 / 驳回。
#[nik::permission(
    path = "/business/flow/task/batch-approve",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/batch-approve")]
pub async fn flow_task_batch_approve(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchApproveReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_approve(&claims, &req).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "018f4b2c-1111-7222-8333-444455550001".to_string(),
            username: Some("d7-flow".to_string()),
            roles: vec!["TEST".to_string()],
            permissions: permissions.iter().map(|item| (*item).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn decision_defaults_to_approve_and_is_case_insensitive() {
        assert_eq!(
            BatchApproveReq::default().decision().expect("默认同意"),
            DECISION_APPROVE
        );
        assert_eq!(
            BatchApproveReq {
                decision: Some(" reject ".to_string()),
                ..Default::default()
            }
            .decision()
            .expect("小写也认"),
            DECISION_REJECT
        );
        let error = BatchApproveReq {
            decision: Some("maybe".to_string()),
            ..Default::default()
        }
        .decision()
        .expect_err("非法决策必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("APPROVE"), "报文：{}", error.msg);
    }

    #[tokio::test]
    async fn batch_approve_requires_permission_and_ids() {
        let error = batch_approve(
            &claims_with(&["business:flow:read"]),
            &BatchApproveReq {
                ids: vec!["018f4b2c-1111-7222-8333-444455550002".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect_err("没有 approve 码必须 403");
        assert_eq!(error.status, 403);
        assert!(error.msg.contains(PERM_APPROVE));

        let empty = batch_approve(&claims_with(&[PERM_APPROVE]), &BatchApproveReq::default())
            .await
            .expect_err("空 ids 必须 400");
        assert_eq!(empty.status, 400);
        assert!(empty.msg.contains("勾选一条"), "报文：{}", empty.msg);
    }
}
