// ============================================================================
// flow_model_graph.rs —— 图与版本快照（序列化 + 发布）：L1 编辑期图的读写、L2 快照读取、节点/边计数、发布落库、草稿复制、在跑实例判定
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

// SCOPE-EXEMPT: 图 / 快照 / 发布全是**定义侧**读写（`biz_flow_node` / `biz_flow_edge` /
// SCOPE-EXEMPT: `biz_flow_node_assignee` / `biz_flow_def_version`），入参一律是 `version_id` / `def_id`
// SCOPE-EXEMPT: 主键，没有面向用户的列表入口；定义是全局配置（批次D · D1 口径，同 `flow_model_def.rs`）。
// SCOPE-EXEMPT: `has_running_instance()` 对运行侧 `biz_flow_instance` 只做 `count(*)` 存在性判定
// SCOPE-EXEMPT: （返回 bool、不产出行集）—— 用于「已发布版本还有在跑实例 → 拒绝改图」，与查看者数据范围无关。

use super::flow_model_base::*;
use crate::business::flow::graph::{
    ApprovalMode, AssigneeKind, AssigneeRef, EdgeSpec, FlowGraph, GatewayMode, NodeSpec, NodeType,
};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 读整图（L1）：**一次读全图**，不做逐节点查询（架构 §8 证据 D：0.250 ms）。
///
/// 三条 SELECT：节点、边（JOIN 出两端 key）、审批人（JOIN node）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；节点类型 / 枚举值非法（脏数据）→ [`DbError`]。
pub async fn load_graph(version_id: Uuid) -> DbResult<FlowGraph> {
    let node_sql = format!(
        "SELECT {NODE_COLUMNS} FROM biz_flow_node n WHERE n.version_id = ?::uuid \
         ORDER BY n.sort_order, n.node_key"
    );
    let node_rows = db().find_with_params(&node_sql, vec![Value::from(&version_id)]).await?;

    let edge_rows = db()
        .find_with_params(
            "SELECT e.id, e.edge_key, f.node_key AS from_key, t.node_key AS to_key, \
                    e.condition_expr::text AS condition_text, e.condition_label, \
                    e.priority, e.is_default \
               FROM biz_flow_edge e \
               JOIN biz_flow_node f ON f.id = e.from_node_id \
               JOIN biz_flow_node t ON t.id = e.to_node_id \
              WHERE e.version_id = ?::uuid \
              ORDER BY e.edge_key",
            vec![Value::from(&version_id)],
        )
        .await?;

    let assignee_rows = db()
        .find_with_params(
            "SELECT a.node_id, a.assignee_type, a.ref_id, a.sort_order \
               FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid \
              ORDER BY a.assignee_type, a.ref_id",
            vec![Value::from(&version_id)],
        )
        .await?;

    let mut assignees_by_node: std::collections::HashMap<Uuid, Vec<AssigneeRef>> = std::collections::HashMap::new();
    for row in &assignee_rows {
        let node_id: Uuid = row.get_typed("node_id")?;
        assignees_by_node.entry(node_id).or_default().push(AssigneeRef {
            kind: row.get_typed("assignee_type")?,
            ref_id: row.get_typed::<Uuid>("ref_id")?.to_string(),
            sort_order: row.get_typed("sort_order")?,
        });
    }

    let mut nodes: Vec<NodeSpec> = Vec::with_capacity(node_rows.len());
    for row in &node_rows {
        let id: Uuid = row.get_typed("id")?;
        let node_type_raw: String = row.get_typed("node_type")?;
        let node_type = NodeType::parse(&node_type_raw)
            .ok_or_else(|| DbError::InvalidValue(format!("node_type 非法：{node_type_raw}")))?;
        let assignee_raw: Option<String> = row.get_typed("assignee_type")?;
        let assignee_type = match assignee_raw.as_deref() {
            None => None,
            Some(text) => Some(
                AssigneeKind::parse(text)
                    .ok_or_else(|| DbError::InvalidValue(format!("assignee_type 非法：{text}")))?,
            ),
        };
        let approval_raw: Option<String> = row.get_typed("approval_mode")?;
        let approval_mode = match approval_raw.as_deref() {
            None => None,
            Some(text) => Some(
                ApprovalMode::parse(text)
                    .ok_or_else(|| DbError::InvalidValue(format!("approval_mode 非法：{text}")))?,
            ),
        };
        let join_raw: String = row.get_typed("join_mode")?;
        let fork_raw: String = row.get_typed("fork_mode")?;
        let config_text: String = row.get_typed("config_text")?;
        let config = serde_json::from_str(&config_text).unwrap_or(serde_json::Value::Null);
        let mut assignees = assignees_by_node.remove(&id).unwrap_or_default();
        assignees.sort_by(|left, right| {
            (left.kind.as_str(), left.ref_id.as_str()).cmp(&(right.kind.as_str(), right.ref_id.as_str()))
        });
        nodes.push(NodeSpec {
            key: row.get_typed("node_key")?,
            name: row.get_typed("node_name")?,
            node_type,
            assignee_type,
            approval_mode,
            approve_ratio: row.get_typed("approve_ratio")?,
            reject_veto: row.get_typed("reject_veto")?,
            join_mode: GatewayMode::parse(&join_raw)
                .ok_or_else(|| DbError::InvalidValue(format!("join_mode 非法：{join_raw}")))?,
            fork_mode: GatewayMode::parse(&fork_raw)
                .ok_or_else(|| DbError::InvalidValue(format!("fork_mode 非法：{fork_raw}")))?,
            form_var_key: row.get_typed("form_var_key")?,
            superior_levels: row.get_typed("superior_levels")?,
            assignees,
            empty_assignee: row.get_typed("empty_assignee")?,
            self_approval: row.get_typed("self_approval")?,
            allow_robot: row.get_typed("allow_robot")?,
            fallback_user_id: row.get_typed("fallback_user_id")?,
            fallback_role_id: row.get_typed("fallback_role_id")?,
            timeout_hours: row.get_typed("timeout_hours")?,
            timeout_action: row.get_typed("timeout_action")?,
            remind_before_hours: row.get_typed("remind_before_hours")?,
            max_remind_times: row.get_typed("max_remind_times")?,
            cc_on_approved: row.get_typed("cc_on_approved")?,
            x: row.get_typed("position_x")?,
            y: row.get_typed("position_y")?,
            sort_order: row.get_typed("sort_order")?,
            subprocess_def_id: row.get_typed("subprocess_def_id")?,
            config,
        });
    }

    let mut edges: Vec<EdgeSpec> = Vec::with_capacity(edge_rows.len());
    for row in &edge_rows {
        let condition_text: Option<String> = row.get_typed("condition_text")?;
        let condition = match condition_text.as_deref() {
            None => None,
            Some(text) if text.trim().is_empty() || text.trim() == "null" => None,
            Some(text) => Some(
                serde_json::from_str(text)
                    .map_err(|error| DbError::InvalidValue(format!("condition_expr 非法：{error}")))?,
            ),
        };
        edges.push(EdgeSpec {
            key: row.get_typed("edge_key")?,
            from: row.get_typed("from_key")?,
            to: row.get_typed("to_key")?,
            condition,
            label: row.get_typed("condition_label")?,
            priority: row.get_typed("priority")?,
            is_default: row.get_typed("is_default")?,
        });
    }

    Ok(FlowGraph { nodes, edges })
}

/// 读发布快照（L2）。未发布 → `None`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn load_snapshot(version_id: Uuid) -> DbResult<Option<String>> {
    load_snapshot_in(db(), version_id).await
}

/// [`load_snapshot`] 的「任意执行器」版本（P-FLOW-1B：事务内读快照判断 `on_complete` / `on_reject`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn load_snapshot_in(ex: &impl SqlExecutor, version_id: Uuid) -> DbResult<Option<String>> {
    let Some(row) = ex
        .query_first_sql(
            "SELECT graph_snapshot::text AS snapshot_text \
               FROM biz_flow_def_version WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&version_id)],
        )
        .await?
    else {
        return Ok(None);
    };
    let text: Option<String> = row.get_typed("snapshot_text")?;
    Ok(text.filter(|value| !value.trim().is_empty()))
}

/// 统计版本下的行数（验收 8：`SELECT count(*)` 与 `node_count/edge_count` 一致）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn count_rows(ex: &impl SqlExecutor, version_id: Uuid) -> DbResult<(i64, i64)> {
    let node_rows = ex
        .query_sql(
            "SELECT count(*) AS total FROM biz_flow_node WHERE version_id = ?::uuid".to_string(),
            vec![Value::from(&version_id)],
        )
        .await?;
    let edge_rows = ex
        .query_sql(
            "SELECT count(*) AS total FROM biz_flow_edge WHERE version_id = ?::uuid".to_string(),
            vec![Value::from(&version_id)],
        )
        .await?;
    let nodes: i64 = match node_rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    let edges: i64 = match edge_rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok((nodes, edges))
}

// ==================== 写路径：定义 ====================

/// 整图覆盖式保存（L1）：先删该版本全部节点（级联删边与审批人），再逐条插入。
///
/// **调用方必须先确认版本是 `DRAFT`**（`handler::save_graph` 做，红线：PUBLISHED 行只读）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn replace_graph(ex: &Transaction, version_id: Uuid, graph: &FlowGraph) -> DbResult<()> {
    // 删节点即级联删 biz_flow_edge / biz_flow_node_assignee（两条 FK 都是 ON DELETE CASCADE）
    ex.execute_with_params(
        "DELETE FROM biz_flow_node WHERE version_id = ?::uuid",
        vec![Value::from(&version_id)],
    )
    .await?;

    // 节点 id 由应用侧生成并保留映射，边与审批人据此挂接
    let mut node_ids: std::collections::HashMap<&str, Uuid> = std::collections::HashMap::new();
    for node in &graph.nodes {
        let id = Uuid::now_v7();
        node_ids.insert(node.key.as_str(), id);
        ex.execute_with_params(
            "INSERT INTO biz_flow_node \
                 (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels, \
                  form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, \
                  self_approval, allow_robot, fallback_user_id, fallback_role_id, \
                  join_mode, fork_mode, timeout_hours, timeout_action, \
                  remind_before_hours, max_remind_times, cc_on_approved, \
                  position_x, position_y, sort_order, subprocess_def_id, config) \
             VALUES (?::uuid, ?::uuid, ?, ?, ?, ?, ?, ?, ?, ?::text::numeric, ?, ?, ?, ?, ?::uuid, ?::uuid, ?, ?, ?, ?, ?, ?, ?, \
                     ?, ?, ?, ?::uuid, ?::jsonb)",
            vec![
                Value::from(&id),
                Value::from(&version_id),
                Value::Text(node.key.clone()),
                Value::Text(node.name.clone()),
                Value::Text(node.node_type.as_str().to_string()),
                opt_text(node.assignee_type.map(|kind| kind.as_str())),
                Value::I16(node.superior_levels),
                opt_text(node.form_var_key.as_deref()),
                opt_text(node.approval_mode.map(|mode| mode.as_str())),
                match node.approve_ratio {
                    Some(ratio) => Value::Text(format!("{ratio:.3}")),
                    None => Value::Null,
                },
                Value::from(node.reject_veto),
                Value::Text(node.empty_assignee.clone()),
                Value::Text(node.self_approval.clone()),
                Value::from(node.allow_robot),
                opt_uuid(node.fallback_user_id),
                opt_uuid(node.fallback_role_id),
                Value::Text(node.join_mode.as_str().to_string()),
                Value::Text(node.fork_mode.as_str().to_string()),
                opt_i32(node.timeout_hours),
                Value::Text(node.timeout_action.clone()),
                opt_i32(node.remind_before_hours),
                Value::I16(node.max_remind_times),
                Value::from(node.cc_on_approved),
                Value::from(node.x),
                Value::from(node.y),
                Value::from(node.sort_order),
                opt_uuid(node.subprocess_def_id),
                Value::Json(node.config.to_string()),
            ],
        )
        .await?;
    }

    for node in &graph.nodes {
        let Some(node_id) = node_ids.get(node.key.as_str()).copied() else {
            continue;
        };
        for assignee in &node.assignees {
            let ref_id = Uuid::parse_str(&assignee.ref_id).map_err(|error| {
                DbError::InvalidValue(format!(
                    "节点 {} 的审批人 ref_id 不是合法 UUID：{}（{error}）",
                    node.key, assignee.ref_id
                ))
            })?;
            ex.execute_with_params(
                "INSERT INTO biz_flow_node_assignee \
                     (node_id, assignee_type, ref_id, sort_order) \
                 VALUES (?::uuid, ?, ?::uuid, ?) ON CONFLICT DO NOTHING",
                vec![
                    Value::from(&node_id),
                    Value::Text(assignee.kind.clone()),
                    Value::from(&ref_id),
                    Value::from(assignee.sort_order),
                ],
            )
            .await?;
        }
    }

    for edge in &graph.edges {
        let (Some(from_id), Some(to_id)) = (node_ids.get(edge.from.as_str()), node_ids.get(edge.to.as_str())) else {
            return Err(DbError::InvalidValue(format!(
                "连线 {} 指向不存在的节点（{} → {}）",
                edge.key, edge.from, edge.to
            )));
        };
        ex.execute_with_params(
            "INSERT INTO biz_flow_edge \
                 (id, version_id, edge_key, from_node_id, to_node_id, condition_expr, \
                  condition_label, priority, is_default) \
             VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::uuid, ?::jsonb, ?, ?, ?)",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&version_id),
                Value::Text(edge.key.clone()),
                Value::from(from_id),
                Value::from(to_id),
                match edge.condition.as_ref() {
                    Some(condition) => Value::Json(condition.to_string()),
                    None => Value::Null,
                },
                opt_text(edge.label.as_deref()),
                Value::from(edge.priority),
                Value::from(edge.is_default),
            ],
        )
        .await?;
    }

    Ok(())
}

/// 发布事务（L1 → L2）：写快照 / hash / 行数，切换 `is_current` 与 `def.current_version_id`。
///
/// # 顺序为什么不能换
///
/// 1. 先把**其它**版本清成 `is_current = FALSE` 并把旧的 PUBLISHED 降为 ARCHIVED
///    （否则 `uk_biz_flow_def_version_current` 这条部分唯一索引会挡下第 2 步）；
/// 2. 再把本版本置 `PUBLISHED + is_current = TRUE` 并写快照；
/// 3. 最后改 `def`（`ck_biz_flow_def_published_has_version` 要求 PUBLISHED 时
///    `current_version_id` 非空 —— 所以必须等第 2 步完成后才能置 PUBLISHED）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn apply_publish(ex: &Transaction, write: &PublishWrite<'_>) -> DbResult<()> {
    let PublishWrite {
        def_id,
        version_id,
        version_no,
        snapshot,
        graph_hash,
        node_count,
        edge_count,
        operator,
    } = *write;
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET is_current = FALSE, \
                version_status = CASE WHEN version_status = 'PUBLISHED' THEN 'ARCHIVED' \
                                      ELSE version_status END, \
                updated_at = now(), update_by = ?::uuid \
          WHERE def_id = ?::uuid AND id <> ?::uuid AND (is_current OR version_status = 'PUBLISHED')",
        vec![Value::from(&operator), Value::from(&def_id), Value::from(&version_id)],
    )
    .await?;

    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET version_status = 'PUBLISHED', is_current = TRUE, graph_hash = ?, \
                graph_snapshot = ?::jsonb, node_count = ?, edge_count = ?, \
                published_at = now(), published_by = ?::uuid, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND def_id = ?::uuid",
        vec![
            Value::Text(graph_hash.to_string()),
            Value::Json(snapshot.to_string()),
            Value::from(node_count),
            Value::from(edge_count),
            Value::from(&operator),
            Value::from(&operator),
            Value::from(&version_id),
            Value::from(&def_id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "UPDATE biz_flow_def \
            SET status = 'PUBLISHED', current_version_id = ?::uuid, current_version_no = ?, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(&version_id),
            Value::from(version_no),
            Value::from(&operator),
            Value::from(&def_id),
        ],
    )
    .await?;
    Ok(())
}

/// 另存为新草稿：把 `source_version_id` 的节点 / 边 / 审批人整份复制到新版本。
///
/// 三步纯 SQL 复制（不把图读进内存再写回）：节点先复制并按 `node_key` 映射新 id，
/// 审批人与边再按 key JOIN 回新节点 —— 200 节点的图也只是 3 条语句。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn copy_version_to_draft(
    ex: &Transaction,
    def_id: Uuid,
    source_version_id: Uuid,
    new_version_id: Uuid,
    change_note: Option<&str>,
    operator: Uuid,
) -> DbResult<(i64, i64)> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_node \
             (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels, \
              form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, \
              self_approval, allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, \
              remind_before_hours, max_remind_times, cc_on_approved, \
              position_x, position_y, sort_order, config) \
         SELECT gen_random_uuid(), ?::uuid, n.node_key, n.node_name, n.node_type, n.assignee_type, \
                n.superior_levels, n.form_var_key, n.approval_mode, n.approve_ratio, n.reject_veto, \
                n.empty_assignee, n.self_approval, n.allow_robot, n.join_mode, n.fork_mode, \
                n.timeout_hours, n.timeout_action, n.remind_before_hours, n.max_remind_times, \
                n.cc_on_approved, n.position_x, n.position_y, n.sort_order, n.config \
           FROM biz_flow_node n WHERE n.version_id = ?::uuid",
        vec![Value::from(&new_version_id), Value::from(&source_version_id)],
    )
    .await?;

    ex.execute_with_params(
        "INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order) \
         SELECT target.id, a.assignee_type, a.ref_id, a.sort_order \
           FROM biz_flow_node_assignee a \
           JOIN biz_flow_node source ON source.id = a.node_id \
           JOIN biz_flow_node target \
             ON target.version_id = ?::uuid AND target.node_key = source.node_key \
          WHERE source.version_id = ?::uuid \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&new_version_id), Value::from(&source_version_id)],
    )
    .await?;

    ex.execute_with_params(
        "INSERT INTO biz_flow_edge \
             (id, version_id, edge_key, from_node_id, to_node_id, condition_expr, \
              condition_label, priority, is_default) \
         SELECT gen_random_uuid(), ?::uuid, e.edge_key, target_from.id, target_to.id, \
                e.condition_expr, e.condition_label, e.priority, e.is_default \
           FROM biz_flow_edge e \
           JOIN biz_flow_node source_from ON source_from.id = e.from_node_id \
           JOIN biz_flow_node source_to ON source_to.id = e.to_node_id \
           JOIN biz_flow_node target_from \
             ON target_from.version_id = ?::uuid AND target_from.node_key = source_from.node_key \
           JOIN biz_flow_node target_to \
             ON target_to.version_id = ?::uuid AND target_to.node_key = source_to.node_key \
          WHERE e.version_id = ?::uuid",
        vec![
            Value::from(&new_version_id),
            Value::from(&new_version_id),
            Value::from(&new_version_id),
            Value::from(&source_version_id),
        ],
    )
    .await?;

    let (node_count, edge_count) = count_rows(ex, new_version_id).await?;
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET node_count = ?, edge_count = ?, change_note = ?, updated_at = now(), \
                update_by = ?::uuid \
          WHERE id = ?::uuid AND def_id = ?::uuid",
        vec![
            Value::from(node_count),
            Value::from(edge_count),
            opt_text(change_note),
            Value::from(&operator),
            Value::from(&new_version_id),
            Value::from(&def_id),
        ],
    )
    .await?;
    Ok((node_count, edge_count))
}

/// 版本是否已被引用（D2 的 `biz_flow_instance.version_id`）。
///
/// 本卡（D1）范围里实例表还不存在，函数先返回 `false` 并在 D2 落地后改为真实查询。
/// **明确写成一个函数**而不是注释：D2 只需要改这一处，不会漏掉「删流程前先查引用」。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn has_running_instance(def_id: Uuid) -> DbResult<bool> {
    // D2 落地：定义侧删流程前必须查「还有没有在跑的单子」（架构 §7.1 #5 的前置校验）。
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance \
              WHERE def_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL",
            vec![Value::from(&def_id)],
        )
        .await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}
