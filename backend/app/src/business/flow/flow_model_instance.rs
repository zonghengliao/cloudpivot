// ============================================================================
// flow_model_instance.rs —— 运行期实例与待办行（状态机）：取实例号、建实例、锁定读、revision 推进、自动通过同步、在跑判定、待办插入/锁定/完结
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

// SCOPE-EXEMPT: 运行侧**引擎内部**取数与写入：全部按主键（`id = ?::uuid`，锁行带 `FOR UPDATE`）
// SCOPE-EXEMPT: 或事务内 `count(*)` 判定（`biz_instance_running` / `sync_auto_passed`），没有列表 / 分页查询，
// SCOPE-EXEMPT: 也没有「查看者」这个入参 —— 把数据范围注入到这里只会打断已被授权的流转（引擎在事务里再裁一次
// SCOPE-EXEMPT: 会让并行分支上由外部门审批的节点走不下去）。可见性判定在两处：`flow_model_inbox::page_instances`
// SCOPE-EXEMPT: （`scope::filter_for(INSTANCE_TARGET)` 拼并集）与 handler 的单条鉴权（`instance_detail` 等）。

use super::flow_model_base::*;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 生成单据号 `FLW-YYYYMMDD-<uuid 末 8 位>`。
///
/// **用 UUIDv7 的末 8 位而不是前 8 位**：v7 的高位就是时间戳，同一毫秒连发的两条
/// 会拿到相同前缀 → 撞 `uk_biz_flow_instance_no`（skill cailu-dev §9.12-3 实测）。
pub fn new_instance_no(now: chrono::DateTime<chrono::Utc>) -> String {
    let id = Uuid::now_v7().to_string();
    let tail: String = id.chars().filter(|c| *c != '-').collect::<String>();
    let tail = tail.get(tail.len().saturating_sub(8)..).unwrap_or("00000000");
    format!("FLW-{}-{}", now.format("%Y%m%d"), tail.to_ascii_uppercase())
}

// ---------- 写：实例 ----------

/// 新建实例（事务内）。
///
/// # 异常情况
///
/// `uk_biz_flow_instance_biz_active` / `uk_biz_flow_instance_no` 冲突 → [`DbError`]。
pub async fn create_instance(ex: &Transaction, new: &NewInstance) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_instance \
             (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
              biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name, \
              initiator_dept_id, form_data, auto_passed_nodes, round_total, revision, \
              started_at, due_at, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?, ?, ?::uuid, ?, ?, 'PENDING', \
                 ?::uuid, ?, ?::uuid, ?::jsonb, 0, 1, 0, now(), ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(new.instance_no.clone()),
            Value::from(&new.def_id),
            Value::from(&new.version_id),
            Value::from(new.version_no),
            Value::Text(new.flow_code.clone()),
            Value::Text(new.flow_name.clone()),
            Value::Text(new.biz_type.clone()),
            opt_uuid(new.biz_id),
            opt_text(new.biz_no.as_deref()),
            Value::Text(new.title.clone()),
            Value::from(&new.initiator_id),
            opt_text(new.initiator_name.as_deref()),
            opt_uuid(new.initiator_dept_id),
            Value::Json(new.form_data.to_string()),
            opt_ts(new.due_at),
            Value::from(&new.initiator_id),
            Value::from(&new.initiator_id),
        ],
    )
    .await?;
    Ok(())
}

/// 读实例（含软删过滤）。不存在 → `None`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_instance(id: Uuid) -> DbResult<Option<InstanceRow>> {
    let sql = format!(
        "SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i \
          WHERE i.id = ?::uuid AND i.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(InstanceRow::from_row).transpose()
}

/// 锁实例行（`SELECT … FOR UPDATE`，锁序第三）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_instance(ex: &Transaction, id: Uuid) -> DbResult<Option<InstanceRow>> {
    let sql = format!("SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i WHERE i.id = ?::uuid FOR UPDATE");
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(InstanceRow::from_row).transpose()
}

/// 按**业务单据**反查审批单（VTRACE-5 全链路视图）。
///
/// # 为什么不复用 `page_instances`
///
/// 那个接口的 `keyword` 只搜 `instance_no` / `title`（`flow_model_inbox.rs`），
/// 业务单号 `biz_no` 搜不到，更没有 `biz_id` 过滤；而全链路视图手上的定位值
/// 只有「来源单据的 uuid / 单号」。所以单独一条按 `biz_id`（或兜底 `biz_no`）命中。
///
/// - `biz_type` 给了就拼进 WHERE：`idx_biz_flow_instance_biz (biz_type, biz_id)` 是复合索引，
///   只给 `biz_id` 用不上前导列（行数小，退化成单表扫描也能接受，但不主动放弃索引）；
/// - 排序 `started_at DESC`：同一张单据可能因撤回重提有多张审批单，**最新的排最前**；
/// - `LIMIT 20`：全链路视图只画最近几轮，不做分页（防脏数据把界面撑爆）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn instances_of_biz(
    biz_type: Option<&str>,
    biz_id: Option<Uuid>,
    biz_no: Option<&str>,
) -> DbResult<Vec<InstanceRow>> {
    let mut sql = format!(
        "SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i \
          WHERE i.deleted_at IS NULL AND (i.biz_id = ?::uuid OR i.biz_no = ?)"
    );
    let mut params = vec![opt_uuid(biz_id), opt_text(biz_no)];
    if let Some(biz_type) = biz_type {
        sql.push_str(" AND i.biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }
    sql.push_str(" ORDER BY i.started_at DESC, i.id DESC LIMIT 20");
    let rows = db().find_with_params(&sql, params).await?;
    rows.iter().map(InstanceRow::from_row).collect()
}

/// 实例 CAS：`revision` 自增并写入状态 / 轮次 / 说明。
///
/// 返回受影响行数：**0 表示 CAS 失败**（别人先推进了同一实例），调用方重新读取后重试一次
/// （架构 §4.5.1 第 ⑦ 步）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn bump_instance(
    ex: &Transaction,
    id: Uuid,
    expected_revision: i32,
    status: Option<&str>,
    result_comment: Option<&str>,
    round_total: i32,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance \
            SET status = COALESCE(?, status), \
                result_comment = COALESCE(?, result_comment), \
                round_total = ?, \
                revision = revision + 1, \
                ended_at = CASE WHEN COALESCE(?, status) = 'PENDING' THEN NULL ELSE now() END, \
                updated_at = now() \
          WHERE id = ?::uuid AND revision = ?",
        vec![
            opt_text(status),
            opt_text(result_comment),
            Value::from(round_total),
            opt_text(status),
            Value::from(&id),
            Value::from(expected_revision),
        ],
    )
    .await
}

/// 实例上**最新一条非空审批意见**（只认真正投过票的待办：`APPROVED` / `REJECTED`）。
///
/// # 用在哪
///
/// 引擎从来只把意见写在待办行（`biz_flow_task.comment`），实例详情页读的却是
/// `biz_flow_instance.result_comment` —— 于是「审批人写了意见」在实例上看不到
/// （FIX-9 ①）。实例落终态时由 [`crate::business::flow::engine`] 调本函数取最后一票的
/// 意见回写。
///
/// # 为什么不直接取「本次这一票的 comment」
///
/// 会签节点最后一票常常只点「同意」不写话，若严格取 `id = 当前待办`，实例的
/// `result_comment` 就又空了；而「最后一条有内容的意见」正是用户在实例详情里想看的。
///
/// # 为什么排除 `CANCELLED`
///
/// 会签提前通过 / 驳回时，其余待办会被置 `CANCELLED` 并写上系统文案
/// （「该节点已满足通过条件，无需再投」）。那不是审批意见，不能盖掉真正的人话。
pub async fn latest_verdict_comment(ex: &Transaction, instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .find_with_params(
            "SELECT t.comment FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.status IN ('APPROVED', 'REJECTED') \
                AND t.comment IS NOT NULL AND btrim(t.comment) <> '' \
              ORDER BY coalesce(t.completed_at, t.created_at) DESC, t.created_at DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("comment"),
    }
}

/// 用 `count(*)` 覆写 `auto_passed_nodes`（**派生值，不是 `+=`**）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn sync_auto_passed(ex: &Transaction, instance_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .find_with_params(
            "UPDATE biz_flow_instance \
                SET auto_passed_nodes = ( \
                        SELECT count(*) FROM biz_flow_instance_node \
                         WHERE instance_id = ?::uuid AND is_auto_passed), \
                    updated_at = now() \
              WHERE id = ?::uuid \
          RETURNING auto_passed_nodes",
            vec![Value::from(&instance_id), Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("auto_passed_nodes"),
    }
}

/// 同一业务单据是否还有在跑的实例（`start` 的前置校验 + 并发兜底）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn biz_instance_running(biz_type: &str, biz_id: Uuid) -> DbResult<bool> {
    biz_instance_running_in(db(), biz_type, biz_id).await
}

/// [`biz_instance_running`] 的「任意执行器」版本（P-FLOW-1B：事务内发起前查重）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn biz_instance_running_in(ex: &impl SqlExecutor, biz_type: &str, biz_id: Uuid) -> DbResult<bool> {
    let total: i64 = ex
        .scalar_sql(
            "SELECT count(*) AS total FROM biz_flow_instance \
              WHERE biz_type = ? AND biz_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(biz_type.to_string()), Value::from(&biz_id)],
        )
        .await?;
    Ok(total > 0)
}

// ---------- 写：实例节点 ----------

/// 插入一条待办行。
///
/// **不用 `ON CONFLICT DO NOTHING`**：三层去重的第 2 层必须真的能挡住重复
/// （验收 2 要贴 23505），并让应用显式走「把 23505 当幂等成功」这条红线路径。
/// 调用方用 [`is_pending_duplicate`] 判定。
///
/// `SKIPPED` 行没有 `PENDING` 槽位，不受部分唯一索引约束。
///
/// # 异常情况
///
/// `uk_biz_flow_task_pending` 冲突 → [`DbError::UniqueViolation`]；其它 → [`DbError`]。
pub async fn insert_task(ex: &Transaction, new: &NewTask) -> DbResult<()> {
    // `completed_at` 必须与 status 同真同假（`ck_biz_flow_task_done`）：
    // PENDING → NULL；其它（SKIPPED）→ now()。用 CASE 而不是绑时间值，保证与库时钟一致。
    ex.execute_with_params(
        "INSERT INTO biz_flow_task \
             (id, instance_id, instance_node_id, node_key, round, task_kind, assignee_id, \
              assignee_name, assignee_source, assignee_source_ref, status, is_added_sign, \
              is_self_approval, is_auto, comment, attachments, transferred_from, due_at, \
              remind_count, created_at, completed_at, assignee_rule_id, \
              escalated_from_role, escalate_reason, assigned_by_rule) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, ?::uuid, ?, ?, ?, ?, ?, \
                 '[]'::jsonb, ?::uuid, ?, 0, now(), \
                 CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END, ?, ?, ?, ?)",
        vec![
            Value::from(&new.id),
            Value::from(&new.instance_id),
            Value::from(&new.instance_node_id),
            Value::Text(new.node_key.clone()),
            Value::from(new.round),
            Value::Text(new.task_kind.clone()),
            Value::from(&new.assignee_id),
            opt_text(new.assignee_name.as_deref()),
            Value::Text(new.assignee_source.clone()),
            opt_uuid(new.assignee_source_ref),
            Value::Text(new.status.clone()),
            Value::from(new.is_added_sign),
            Value::from(new.is_self_approval),
            Value::from(new.is_auto),
            opt_text(new.comment.as_deref()),
            opt_uuid(new.transferred_from),
            opt_ts(new.due_at),
            Value::Text(new.status.clone()),
            opt_text(new.assignee_rule_id.as_deref()),
            opt_text(new.escalated_from_role.as_deref()),
            opt_text(new.escalate_reason.as_deref()),
            opt_text(new.assigned_by_rule.as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 锁待办行（`SELECT … FOR UPDATE`，锁序第一）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_task(ex: &Transaction, id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM biz_flow_task t WHERE t.id = ?::uuid FOR UPDATE");
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TaskRow::from_row).transpose()
}

/// 读待办（不加锁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_task(id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM biz_flow_task t WHERE t.id = ?::uuid");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TaskRow::from_row).transpose()
}

/// 结束一条待办（写状态 / 意见 / 完成时间）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn finish_task(
    ex: &Transaction,
    id: Uuid,
    status: &str,
    comment: Option<&str>,
    is_auto: bool,
) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_task \
            SET status = ?, comment = COALESCE(?, comment), is_auto = is_auto OR ?, \
                completed_at = CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END \
          WHERE id = ?::uuid",
        vec![
            Value::Text(status.to_string()),
            opt_text(comment),
            Value::from(is_auto),
            Value::Text(status.to_string()),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}
