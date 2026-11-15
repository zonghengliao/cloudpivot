//! P3-06 存量签名模式冻结（V0339 的**已发布版本**部分）。
//!
//! # 为什么这段不能写成 SQL 迁移
//!
//! `V0339__approval_sign_mode_freeze.up.sql` 能安全处理**草稿版本**的行（草稿没人读），
//! 但已发布版本有两个 SQL 改不动的东西：
//!
//! 1. **快照才是实例真正读的那份图**（`engine::load_snapshot` 读
//!    `biz_flow_def_version.graph_snapshot`）。只改 `biz_flow_node` 行的话，已发布流程
//!    起新实例时依旧拿到 `approval_mode = NULL`，在「不填 = 或签」的新默认下会**静默
//!    从全员签降级成或签** —— 正是决策 D2 要避免的事。
//! 2. **`graph_hash` 是「从行重建出的 canonical JSON」的 sha256**。只改行 = 行 hash 与
//!    发布时写死的 hash 对不上，`flow::maintain` 的「审批图快照自检」（每天 03:30）
//!    会把这些版本逐条报成「发布版本的 node/edge 行被直接修改」并写审计 —— 天天误报。
//!
//! 所以这里在启动期（`bootstrap`，与「补齐当年会计期间」同款：写不进迁移的运行期数据修复）
//! 做三件事的**原子替换**：解析出 Graph → 按 D2 口径冻结 `approval_mode` → 用**同一套**
//! `snapshot_json()` / `graph_hash()` 重写快照与 hash → 同步改行。三者一致，自检不报警。
//!
//! # 口径（与 `graph::freeze_sign_mode` / V0339 逐字同义）
//!
//! 候选人数 ≥ 2 → `ALL`（保持历史「全员签」行为）；< 2 或算不出来 → `ANY`。
//! 候选人数按节点的 `assignee_type` 现算：`USER` 去重人数、`ROLE` 启用成员数、
//! `DEPT_LEADER` 部门负责人数、`ADMIN` 启用超管数、其余单值来源记 1、算不出的记 NULL。
//!
//! # 幂等
//!
//! 只处理 `node_type = 'APPROVAL' AND approval_mode IS NULL` 的行；第一次跑完就没有 NULL 了，
//! 之后每次启动只付一条索引扫描的代价（返回 0 行）。重放同一版本得到同一份快照（纯函数）。
//!
// SCOPE-EXEMPT: 流程定义（biz_flow_node / biz_flow_def_version / biz_flow_instance_node）
// SCOPE-EXEMPT: 是全平台共用的规则与版本快照，不是「谁的业务数据」，按部门裁剪会让「我的单
// SCOPE-EXEMPT: 该走哪个流程」都查不出来；与 flow/model.rs 的豁免口径一致（闸② 同一条理由）。

use nik::prelude::*;
use nik::uuid::Uuid;
use std::collections::BTreeMap;

use crate::business::flow::graph::{self, ApprovalMode, NodeType};
use crate::business::flow::model;

/// 一轮冻结的统计（写启动日志 + 证据文档）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreezeReport {
    /// 处理了多少个已发布/归档版本。
    pub versions: usize,
    /// 冻结了多少个节点。
    pub nodes: usize,
    /// 其中写成 `ALL`（候选人数 ≥ 2）。
    pub all: usize,
    /// 其中写成 `ANY`（候选人数 < 2 或算不出来）。
    pub any: usize,
}

impl FreezeReport {
    /// 是否什么都没做（已冻结过）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.versions == 0 && self.nodes == 0
    }
}

/// 候选人数口径（**与 `V0339` 的 SQL 同一份语义**，改动必须两边一起改）。
const SQL_PENDING: &str = "SELECT n.id AS node_id, n.version_id AS version_id, n.node_key AS node_key, \
        CASE n.assignee_type \
            WHEN 'USER' THEN (SELECT count(DISTINCT a.ref_id) FROM biz_flow_node_assignee a \
                               WHERE a.node_id = n.id AND a.assignee_type = 'USER') \
            WHEN 'ROLE' THEN (SELECT count(DISTINCT ur.user_id) \
                                FROM biz_flow_node_assignee a \
                                JOIN sys_role r ON r.id = a.ref_id \
                                              AND r.deleted_at IS NULL AND r.status = 1 \
                                JOIN sys_user_role ur ON ur.role_id = a.ref_id \
                                                     AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
                                JOIN sys_user u ON u.id = ur.user_id \
                                               AND u.deleted_at IS NULL AND u.status = 1 \
                               WHERE a.node_id = n.id AND a.assignee_type = 'ROLE') \
            WHEN 'DEPT_LEADER' THEN (SELECT count(DISTINCT d.leader_id) \
                                       FROM biz_flow_node_assignee a \
                                       JOIN sys_dept d ON d.id = a.ref_id AND d.leader_id IS NOT NULL \
                                      WHERE a.node_id = n.id AND a.assignee_type = 'DEPT_LEADER') \
            WHEN 'ADMIN' THEN (SELECT count(DISTINCT u.id) FROM sys_user u \
                                 JOIN sys_user_role ur ON ur.user_id = u.id \
                                 JOIN sys_role r ON r.id = ur.role_id \
                                WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL \
                                  AND u.deleted_at IS NULL AND u.status = 1) \
            WHEN 'INITIATOR' THEN 1 \
            WHEN 'INITIATOR_SUPERIOR' THEN 1 \
            WHEN 'TASK_OWNER' THEN 1 \
            WHEN 'TASK_CREATOR' THEN 1 \
            WHEN 'TASK_ASSIGNER' THEN 1 \
            ELSE NULL \
        END AS candidates \
   FROM biz_flow_node n \
   JOIN biz_flow_def_version v ON v.id = n.version_id \
  WHERE n.node_type = 'APPROVAL' AND n.approval_mode IS NULL \
    AND v.version_status IN ('PUBLISHED', 'ARCHIVED')";

/// 冻结一个版本里待处理的节点（`node_key` → 候选人数）。
struct Pending {
    /// 节点行 id。
    node_id: Uuid,
    /// 节点 key。
    node_key: String,
    /// 候选人数（`None` = 算不出来）。
    candidates: Option<usize>,
}

/// 冻结所有已发布 / 归档版本里 `approval_mode` 为空的 `APPROVAL` 节点。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；单个版本失败**不阻断**其它版本（记 `tracing::error!` 后继续），
/// 返回的统计只包含成功的部分 —— 启动引导不允许因为一个坏版本让整个服务起不来。
pub async fn freeze_published_versions() -> DbResult<FreezeReport> {
    let rows = db().find_with_params(SQL_PENDING, Vec::new()).await?;
    if rows.is_empty() {
        return Ok(FreezeReport::default());
    }

    let mut by_version: BTreeMap<Uuid, Vec<Pending>> = BTreeMap::new();
    for row in &rows {
        let version_id: Uuid = row.get_typed("version_id")?;
        let node_id: Uuid = row.get_typed("node_id")?;
        let node_key: String = row.get_typed("node_key")?;
        let candidates: Option<i64> = row.get_typed("candidates")?;
        by_version.entry(version_id).or_default().push(Pending {
            node_id,
            node_key,
            candidates: candidates.map(|value| value.max(0) as usize),
        });
    }

    let mut report = FreezeReport::default();
    for (version_id, pending) in by_version {
        match freeze_one(version_id, &pending).await {
            Ok((all, any)) => {
                report.versions += 1;
                report.nodes += pending.len();
                report.all += all;
                report.any += any;
            }
            Err(error) => tracing::error!(
                version_id = %version_id,
                error = %error,
                "P3-06 签名模式冻结：该版本失败，跳过（下次启动重试）"
            ),
        }
    }
    Ok(report)
}

/// 冻结单个版本：快照 + hash + 行 + 运行中实例节点，四者一致。
///
/// 返回 `(写成 ALL 的节点数, 写成 ANY 的节点数)`。
async fn freeze_one(version_id: Uuid, pending: &[Pending]) -> DbResult<(usize, usize)> {
    // ① 从**行**重建图（发布时用的同一入口），再按 D2 口径改模式
    let mut graph = model::load_graph(version_id).await?;
    let mut modes: Vec<(Uuid, String, ApprovalMode)> = Vec::with_capacity(pending.len());
    let mut all = 0usize;
    let mut any = 0usize;
    for item in pending {
        let mode = graph::freeze_sign_mode(item.candidates);
        let Some(node) = graph
            .nodes
            .iter_mut()
            .find(|node| node.key == item.node_key && node.node_type == NodeType::Approval)
        else {
            tracing::warn!(
                version_id = %version_id,
                node_key = %item.node_key,
                "P3-06 签名模式冻结：行里有该节点但重建的图里没有，跳过"
            );
            continue;
        };
        node.approval_mode = Some(mode);
        match mode {
            ApprovalMode::All => all += 1,
            _ => any += 1,
        }
        modes.push((item.node_id, item.node_key.clone(), mode));
    }
    if modes.is_empty() {
        return Ok((0, 0));
    }

    // ② 快照 + hash 用**发布期同一套**序列化重算（纯函数，同一份行永远得到同一份 hash）
    let snapshot = graph.snapshot_json();
    let hash = graph.graph_hash();
    let node_count = graph.nodes.len() as i32;
    let edge_count = graph.edges.len() as i32;

    let tx = db().begin().await?;
    tx.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET graph_snapshot = ?::jsonb, graph_hash = ?, node_count = ?, edge_count = ?, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            // 与 `flow_model_graph::apply_publish` 同一个绑定方式：jsonb 列不能绑 Text
            Value::Json(snapshot),
            Value::Text(hash),
            Value::from(node_count),
            Value::from(edge_count),
            Value::from(&version_id),
        ],
    )
    .await?;

    // ③ 行（编辑器 / 下次发布读这里）
    for (node_id, _, mode) in &modes {
        tx.execute_with_params(
            "UPDATE biz_flow_node SET approval_mode = ? WHERE id = ?::uuid",
            vec![Value::Text(mode.as_str().to_string()), Value::from(node_id)],
        )
        .await?;
    }

    // ④ 运行中实例的节点行（vote() 读这里判票）：同样冻结，正在跑的流程签法不被打断
    for (_, node_key, mode) in &modes {
        tx.execute_with_params(
            "UPDATE biz_flow_instance_node SET approval_mode = ?, updated_at = now() \
              WHERE node_key = ? AND approval_mode IS NULL AND instance_id IN ( \
                    SELECT id FROM biz_flow_instance WHERE version_id = ?::uuid)",
            vec![
                Value::Text(mode.as_str().to_string()),
                Value::Text(node_key.clone()),
                Value::from(&version_id),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok((all, any))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 口径守卫：这段 SQL 的 WHERE 必须只锁「已发布/归档 + APPROVAL + 空模式」，
    /// 少一个条件就会误伤（比如把 COUNTERSIGN 也改了、或反复重写已冻结的行）。
    #[test]
    fn pending_sql_only_targets_unfrozen_published_approvals() {
        for needle in [
            "n.node_type = 'APPROVAL'",
            "n.approval_mode IS NULL",
            "v.version_status IN ('PUBLISHED', 'ARCHIVED')",
            "WHEN 'ROLE' THEN",
            "WHEN 'ADMIN' THEN",
            "ELSE NULL",
        ] {
            assert!(SQL_PENDING.contains(needle), "SQL 少了关键条件：{needle}");
        }
        // 草稿由 V0339 的 SQL 负责，这段不许碰（否则启动期会把草稿也重写一遍）
        assert!(
            !SQL_PENDING.contains("'DRAFT'"),
            "启动期冻结只处理已发布/归档版本，草稿归 V0339"
        );
    }
}
