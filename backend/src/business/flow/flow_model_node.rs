// ============================================================================
// flow_model_node.rs —— 节点类型与节点行流转：节点读/锁、激活与结束、计数回写、等待态、按实例取节点、撤销活跃节点、轮次与到达判定
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

// SCOPE-EXEMPT: 节点行流转只按主键 / `instance_id` 取数：`find_node` / `lock_node` 是单行
// SCOPE-EXEMPT: （`LIMIT 1` / `FOR UPDATE`），`max_round` / `node_arrived` 是 `max()` / `count(*)` 的单值判定，
// SCOPE-EXEMPT: `nodes_of` 是按**已鉴权实例 id** 扇出的子行（不是独立的列表入口）；入参全部来自引擎内部，
// SCOPE-EXEMPT: 没有查看者身份。可见性口径同 `flow_model_instance.rs`：inbox::page_instances 拼并集 +
// SCOPE-EXEMPT: handler 单条鉴权。拆分前这些查询同样在 `model.rs` 的文件头豁免半径内。

use super::flow_model_base::*;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 幂等地登记「本轮到达过这个节点」。
///
/// `ON CONFLICT (instance_id, node_key, round) DO UPDATE` 让重复激活变成一次无害的
/// `updated_at` 刷新（**幂等**而不报 23505）—— 与 `biz_flow_task` 的插入刻意不同：
/// 那边要求「23505 当幂等成功」的红线走显式错误分支，这边没有外部可观察语义。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn touch_node(ex: &Transaction, new: &NewNode) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_instance_node \
             (id, instance_id, node_id, node_key, node_name, node_type, round, status, \
              approval_mode, approve_ratio, reject_veto, approver_count, approved_count, \
              rejected_count, is_auto_passed, created_at, updated_at) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?, 'PENDING', ?, ?::text::numeric, ?, 0, 0, 0, \
                 FALSE, now(), now()) \
         ON CONFLICT (instance_id, node_key, round) DO UPDATE SET updated_at = now()",
        vec![
            Value::from(&new.id),
            Value::from(&new.instance_id),
            opt_uuid(new.node_id),
            Value::Text(new.node_key.clone()),
            Value::Text(new.node_name.clone()),
            Value::Text(new.node_type.clone()),
            Value::from(new.round),
            opt_text(new.approval_mode.as_deref()),
            new.approve_ratio
                .map(|ratio| Value::Text(format!("{ratio:.3}")))
                .unwrap_or(Value::Null),
            new.reject_veto.map(Value::from).unwrap_or(Value::Null),
        ],
    )
    .await?;
    Ok(())
}

/// 读某轮次的节点行（不加锁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_node(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<Option<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n \
          WHERE n.instance_id = ?::uuid AND n.node_key = ? AND n.round = ?"
    );
    let rows = ex
        .find_with_params(
            &sql,
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    rows.first().map(NodeRow::from_row).transpose()
}

/// 锁节点行（`SELECT … FOR UPDATE`，锁序第二）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_node(ex: &Transaction, id: Uuid) -> DbResult<Option<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n WHERE n.id = ?::uuid FOR UPDATE"
    );
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(NodeRow::from_row).transpose()
}

/// 把节点置为 `ACTIVE` 并写应到人数。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn activate_node_row(ex: &Transaction, id: Uuid, approver_count: i32) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = 'ACTIVE', approver_count = ?, activated_at = now(), \
                ended_at = NULL, duration_ms = NULL, updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(approver_count), Value::from(&id)],
    )
    .await?;
    Ok(())
}

/// 结束节点：写终态 / 计数 / 结束时间 / 耗时 / 是否自动通过。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn end_node_row(
    ex: &Transaction,
    id: Uuid,
    status: &str,
    tally: Tally,
    taken_edge_key: Option<&str>,
    is_auto_passed: bool,
) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = ?, approved_count = ?, rejected_count = ?, \
                approver_count = CASE WHEN ? > 0 THEN ? ELSE approver_count END, \
                taken_edge_key = COALESCE(?, taken_edge_key), \
                is_auto_passed = ?, \
                activated_at = COALESCE(activated_at, now()), \
                ended_at = now(), \
                duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::Text(status.to_string()),
            Value::from(tally.approved),
            Value::from(tally.rejected),
            Value::from(tally.total),
            Value::from(tally.total),
            opt_text(taken_edge_key),
            Value::from(is_auto_passed),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 覆写节点上的票数展示冗余（**绝对值，不是 `+=`**，架构 D15）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn update_node_counts(ex: &Transaction, id: Uuid, tally: Tally) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET approved_count = ?, rejected_count = ?, \
                approver_count = CASE WHEN ? > 0 THEN ? ELSE approver_count END, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::from(tally.approved),
            Value::from(tally.rejected),
            Value::from(tally.total),
            Value::from(tally.total),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 把节点标成「等人齐」（汇聚点 `AND` 未到齐）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn mark_node_waiting(ex: &Transaction, id: Uuid) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = CASE WHEN status = 'ACTIVE' THEN status ELSE 'PENDING' END, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await?;
    Ok(())
}

/// 实例下所有节点行（按轮次、到达顺序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn nodes_of(instance_id: Uuid) -> DbResult<Vec<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n \
          WHERE n.instance_id = ?::uuid ORDER BY n.round, n.created_at, n.node_key"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(NodeRow::from_row).collect()
}

/// 掐断实例里所有 `ACTIVE` 节点（退回 / 撤回 / 终止时用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_active_nodes(
    ex: &Transaction,
    instance_id: Uuid,
    except_node_id: Option<Uuid>,
) -> DbResult<u64> {
    let mut sql = String::from(
        "UPDATE biz_flow_instance_node \
            SET status = 'CANCELLED', ended_at = now(), \
                duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint, \
                updated_at = now() \
          WHERE instance_id = ?::uuid AND status IN ('ACTIVE','PENDING')",
    );
    let mut params = vec![Value::from(&instance_id)];
    if let Some(node_id) = except_node_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&node_id));
    }
    ex.execute_with_params(&sql, params).await
}

/// 实例当前最大轮次。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn max_round(ex: &Transaction, instance_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .find_with_params(
            "SELECT COALESCE(max(round), 1) AS max_round FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(1),
        Some(row) => row.get_typed("max_round"),
    }
}

/// 某节点在某轮次的行是否存在且为 `APPROVED`/`SKIPPED`（汇聚判定用，**必须带 round**）。
///
/// 这是坑 4 的核心：不带 `round` 就会读到**上一轮**的通过记录，退回后汇聚点直接放行。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn node_arrived(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<bool> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND node_key = ? AND round = ? \
                AND status IN ('APPROVED','SKIPPED')",
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

// ---------- 写：待办 ----------
