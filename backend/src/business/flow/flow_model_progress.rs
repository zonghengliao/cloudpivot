// ============================================================================
// flow_model_progress.rs —— 进度与参与人视图：节点计数与已办数、上一节点锚点、跳线痕迹、时间线、抄送写入与读取、待办候选/抄送人、发起人画像、业务单的流程任务列表
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::flow_model_base::*;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 现算票数（**唯一的票数来源**，`COUNT(*) FILTER`，架构 D15 第 3 层）。
///
/// `total` 只统计「在册票」= `PENDING + APPROVED + REJECTED`：
/// `SKIPPED`（自审跳过）与 `TRANSFERRED`（已转办）不是票，`CANCELLED` 是已作废的票。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn tally_of_node(ex: &Transaction, node_id: Uuid, round: i32) -> DbResult<Tally> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) FILTER (WHERE status = 'APPROVED') AS approved, \
                    count(*) FILTER (WHERE status = 'REJECTED') AS rejected, \
                    count(*) FILTER (WHERE status = 'PENDING') AS pending, \
                    count(*) FILTER (WHERE status IN ('PENDING','APPROVED','REJECTED')) AS total \
               FROM biz_flow_task \
              WHERE instance_node_id = ?::uuid AND round = ?",
            vec![Value::from(&node_id), Value::from(round)],
        )
        .await?;
    match rows.first() {
        None => Ok(Tally::default()),
        Some(row) => Ok(Tally {
            approved: row.get_typed("approved")?,
            rejected: row.get_typed("rejected")?,
            pending: row.get_typed("pending")?,
            total: row.get_typed("total")?,
        }),
    }
}

/// 实例里「已经表过态」的票数（撤回的「零人已审」判定，架构 §4.7）。
///
/// **不算** `SKIPPED`（自动跳过的人没审过）与 `TRANSFERRED`（转办出去的人也没审）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn reviewed_count(ex: &Transaction, instance_id: Uuid) -> DbResult<i64> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED')",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("total"),
    }
}

// ---------- 读：留痕挂点（C1 审批结论未落单据） ----------

/// 取实例上最新的一个节点行（留痕的挂点）。
///
/// # 为什么需要它
///
/// `biz_flow_task.instance_node_id` 是 **NOT NULL + 外键**：审批回调要在实例上补一条
/// 「目标已不可改，跳过落状态」的留痕，必须先知道挂在哪一个节点上。挂最新的节点
/// （`round` 最大、`created_at` 最新）在时间线上就是「刚结束的那一步」，语义最贴近。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。实例一条节点行都没有时返回 `Ok(None)`（调用方记 WARN 后放弃留痕）。
pub async fn last_node_anchor(instance_id: Uuid) -> DbResult<Option<NodeAnchor>> {
    let rows = db()
        .find_with_params(
            "SELECT n.id, n.node_key, n.round FROM biz_flow_instance_node n \
              WHERE n.instance_id = ?::uuid \
              ORDER BY n.round DESC, n.created_at DESC, n.id DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(NodeAnchor {
            id: row.get_typed("id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
        })),
    }
}

// SCOPE-EXEMPT: 按 `instance_id` 的 `count(*)` 做存在性判定，不是列表查询；
// 与 `last_node_anchor` 同一调用链（审批回调 / 系统身份）。
/// 这个实例上是否已经留过「审批结论未落单据」的痕迹（幂等判据）。
///
/// 同一个实例只留一条：重复回调 / 重放不重复刷痕（`SKIPPED` 行不走
/// `uk_biz_flow_task_pending`，没有库层唯一键兜底，只能在这里判）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（调用方当作「留痕没写成」，只记 WARN，不影响审批结论）。
pub async fn has_skip_trace(instance_id: Uuid, prefix: &str) -> DbResult<bool> {
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.status = 'SKIPPED' \
                AND t.is_auto = true AND t.comment LIKE ?::text",
            vec![
                Value::from(&instance_id),
                Value::Text(format!("%{prefix}%")),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(false),
        Some(row) => {
            let total: i64 = row.get_typed("total")?;
            Ok(total > 0)
        }
    }
}

/// 实例的审批时间线（全部轮次，含 `SKIPPED` / `TRANSFERRED` / `CANCELLED` 留痕）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn timeline(instance_id: Uuid) -> DbResult<Vec<TaskRow>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM biz_flow_task t \
          WHERE t.instance_id = ?::uuid ORDER BY t.round, t.created_at, t.id"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(TaskRow::from_row).collect()
}

/// 我在这个实例上的待办 id（详情页的「可用动作」）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_task_of(instance_id: Uuid, user_id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM biz_flow_task t \
          WHERE t.instance_id = ?::uuid AND t.assignee_id = ?::uuid AND t.status = 'PENDING' \
          ORDER BY t.round DESC, t.created_at DESC LIMIT 1"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id), Value::from(&user_id)])
        .await?;
    rows.first().map(TaskRow::from_row).transpose()
}

// ---------- 写：抄送 ----------

/// 新建抄送行。///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（重复由 `uk_biz_flow_cc` + `ON CONFLICT DO NOTHING` 吞掉）。
pub async fn insert_cc(ex: &Transaction, new: &NewCc) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_cc \
             (id, instance_id, node_key, round, receiver_id, receiver_name, source_type, \
              source_ref, is_read, created_at) \
         VALUES (?::uuid, ?::uuid, ?, ?, ?::uuid, ?, ?, ?::uuid, FALSE, now()) \
         ON CONFLICT (instance_id, node_key, round, receiver_id) DO NOTHING",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&new.instance_id),
            Value::Text(new.node_key.clone()),
            Value::from(new.round),
            Value::from(&new.receiver_id),
            opt_text(new.receiver_name.as_deref()),
            Value::Text(new.source_type.clone()),
            opt_uuid(new.source_ref),
        ],
    )
    .await?;
    Ok(())
}

/// 实例的抄送列表。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cc_of(instance_id: Uuid) -> DbResult<Vec<CcRow>> {
    let sql = format!(
        "SELECT {CC_COLUMNS} FROM biz_flow_cc c \
          WHERE c.instance_id = ?::uuid ORDER BY c.created_at"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(CcRow::from_row).collect()
}

/// 实例里所有 `PENDING` 待办的审批人（撤回时通知他们）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn pending_assignees(ex: &Transaction, instance_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT DISTINCT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed("assignee_id")?);
    }
    out.sort();
    Ok(out)
}

/// 某节点某轮次的抄送收件人（抄送节点落库后发通知用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cc_receivers(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT receiver_id FROM biz_flow_cc \
              WHERE instance_id = ?::uuid AND node_key = ? AND round = ?",
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed("receiver_id")?);
    }
    out.sort();
    Ok(out)
}

/// 按 id 取用户（未软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_brief(id: Uuid) -> DbResult<Option<UserBrief>> {
    let rows = db()
        .find_with_params(
            "SELECT u.id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, \
                    u.user_type, (u.status = 1) AS active \
               FROM sys_user u WHERE u.id = ?::uuid AND u.deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let user_type: String = row.get_typed("user_type")?;
            Ok(Some(UserBrief {
                id: row.get_typed("id")?,
                name: row.get_typed("name")?,
                is_robot: user_type.eq_ignore_ascii_case("ROBOT"),
                active: row.get_typed("active")?,
            }))
        }
    }
}

/// 按 id 取用户姓名与部门（发起时冻结发起人快照）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_profile(id: Uuid) -> DbResult<Option<(String, Option<Uuid>)>> {
    user_profile_in(db(), id).await
}

/// [`user_profile`] 的「任意执行器」版本（P-FLOW-1B：事务内发起审批单时取发起人快照）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_profile_in(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<(String, Option<Uuid>)>> {
    let Some(row) = ex
        .query_first_sql(
            "SELECT COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, u.dept_id \
               FROM sys_user u WHERE u.id = ?::uuid AND u.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((row.get_typed("name")?, row.get_typed("dept_id")?)))
}

// ---------- 参数小工具 ----------

/// 某个流程实例下由 `TASK_NODE` 生成的全部任务（**只读 `source='FLOW'` 的行**）。
///
/// # 为什么按 `flow_instance_id` 直查而不是走 `parent_id` 树
///
/// 流程任务**不写** `biz_task.parent_id`（决策 4：那是「子任务」语义）。三列 + 部分索引
/// `idx_biz_task_flow_instance` 让「本单生成了哪些活」变成一次索引扫描。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn flow_tasks_of(instance_id: Uuid) -> DbResult<Vec<FlowTaskRow>> {
    let rows = db()
        .find_with_params(
            "SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, \
                    t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
                    t.need_review, t.due_date, t.flow_node_key, t.flow_round, t.created_at \
               FROM biz_task t \
               LEFT JOIN sys_user u ON u.id = t.owner_id \
              WHERE t.flow_instance_id = ?::uuid \
                AND t.source::text = 'FLOW' \
                AND t.deleted_at IS NULL \
              ORDER BY t.created_at, t.id",
            vec![Value::from(&instance_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(FlowTaskRow {
                id: row.get_typed("id")?,
                task_no: row.get_typed("task_no")?,
                title: row.get_typed("title")?,
                status: row.get_typed("status")?,
                priority: row.get_typed("priority")?,
                progress: row.get_typed::<i16>("progress").unwrap_or(0),
                owner_id: row.get_typed("owner_id")?,
                owner_name: row.get_typed("owner_name").unwrap_or_default(),
                need_review: row.get_typed("need_review").unwrap_or(false),
                due_date: row
                    .get_typed::<Option<chrono::NaiveDate>>("due_date")
                    .ok()
                    .flatten(),
                flow_node_key: row
                    .get_typed::<Option<String>>("flow_node_key")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                flow_round: row
                    .get_typed::<Option<i32>>("flow_round")
                    .ok()
                    .flatten()
                    .unwrap_or(1),
                created_at: row.get_typed("created_at")?,
            })
        })
        .collect()
}

// ==================== 单元测试 ====================
