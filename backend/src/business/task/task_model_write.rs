// ============================================================================
// task_model_write.rs —— 写入：建/改/删任务、协作人、评论、进度、转派
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_base::*;
use super::task_model_rows::*;

/// 新建任务（同事务内生成 `task_no`）。
///
/// # 编号生成（`TSK-YYYYMM-NNNNNN`）
///
/// 1. `pg_advisory_xact_lock(7700000000 + YYYYMM)` —— 事务级咨询锁，**按月串行化**，
///    提交/回滚自动释放。并发创建时后到者等待，不会拿到同一个序号；
/// 2. `max(月内已有最大序号) + 1` —— 含软删行（编号不复用）；
/// 3. 落库。
///
/// 不用「全局 sequence」是因为编号要求按月重置；不用「先查后插不加锁」是因为并发下
/// 两个请求会算出同一个序号（虽然唯一索引会拦，但用户看到的是 500 而不是成功）。
/// 也不额外建计数表 —— 7 张表的模型是老大定稿的，不为编号再开一张表。
///
/// # 返回值
///
/// 生成的任务编号。
///
/// # 异常情况
///
/// - 数据库异常（含 `uk_biz_task_task_no` 冲突）→ [`DbError`]。
pub async fn create_task(
    ex: &Transaction,
    new: &NewTask,
    operator: Uuid,
    month: &str,
) -> DbResult<String> {
    let task_no = next_task_no(ex, month).await?;

    ex.execute_with_params(
        "INSERT INTO biz_task \
                 (id, task_no, title, description, status, priority, progress, need_review, \
                  owner_id, creator_id, assigner_id, dept_id, parent_id, project_id, \
                  doc_type, doc_id, \
                  start_date, due_date, done_at, estimate_hours, actual_hours, source, \
                  cancel_reason, reject_reason, approval_instance_id, \
                  created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, 'TODO', ?, 0, ?, \
                     ?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid, \
                     ?, ?::uuid, \
                     ?::date, ?::date, NULL, ?::numeric, NULL, ?, \
                     NULL, NULL, NULL, \
                     now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(task_no.clone()),
            Value::Text(new.title.clone()),
            Value::from(new.description.clone()),
            Value::Text(new.priority.clone()),
            Value::Bool(new.need_review),
            Value::from(&new.owner_id),
            Value::from(&new.creator_id),
            Value::from(new.assigner_id.as_ref()),
            Value::from(new.dept_id.as_ref()),
            Value::from(new.parent_id.as_ref()),
            Value::from(new.project_id.as_ref()),
            // P2-9：类型与 id 一起落（库侧 ck_biz_task_doc_pair 兜底成对）
            Value::from(new.doc_type.clone()),
            Value::from(new.doc_id.as_ref()),
            Value::from(new.start_date),
            Value::from(new.due_date),
            Value::from(new.estimate_hours),
            Value::Text(new.source.clone()),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;

    if new.add_creator_as_collaborator {
        add_member(ex, new.id, new.creator_id, "COLLABORATOR", operator).await?;
    }
    Ok(task_no)
}

/// 生成下一个任务编号（**必须在事务内调用**：依赖 `pg_advisory_xact_lock`）。
///
/// `pub(crate)`：流程引擎的 `flow::task_node::spawn` 要用同一套取号逻辑
/// （`uk_biz_task_task_no` 撞库重试也复用同一个口径），不许在流程侧另写一份 MAX+1。
// SCOPE-EXEMPT: 取号必须跨**全表** MAX+1（表上有唯一索引 uk_biz_task_task_no），注入数据范围会让不同部门算出同一个号 → 撞唯一索引/串号，这是取号语义本身的硬要求；返回一个编号字符串、不产出行集；调用 create_task（task_model_write.rs:41）← handler.rs:314（business:task:create）与流程引擎 flow/task_node.rs:221（无操作人）。
pub(crate) async fn next_task_no(ex: &Transaction, month: &str) -> DbResult<String> {
    // 咨询锁键：7700000000 + YYYYMM（固定命名空间，避免与其它模块的 advisory lock 撞号）
    let month_number: i64 = month.parse().unwrap_or(0);
    let lock_key = 7_700_000_000_i64 + month_number;
    ex.find_with_params(
        "SELECT 1 AS locked FROM pg_advisory_xact_lock(?) AS l",
        vec![Value::I64(lock_key)],
    )
    .await?;

    let pattern = format!("TSK-{month}-%");
    let rows = ex
        .find_with_params(
            "SELECT COALESCE(max(substring(task_no from 12)::bigint), 0) + 1 AS next_no \
               FROM biz_task \
              WHERE task_no LIKE ? AND substring(task_no from 12) ~ '^[0-9]+$'",
            vec![Value::Text(pattern)],
        )
        .await?;
    let next_no: i64 = match rows.first() {
        Some(row) => row.get_typed("next_no")?,
        None => 1,
    };
    Ok(format_task_no(month, next_no))
}

/// 协作人 / 关注者入库（`ON CONFLICT DO NOTHING`：重复添加幂等）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn add_member(
    ex: &Transaction,
    task_id: Uuid,
    user_id: Uuid,
    role: &str,
    operator: Uuid,
) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_task_member (task_id, user_id, role, created_at, create_by) \
              VALUES (?::uuid, ?::uuid, ?, now(), ?::uuid) \
         ON CONFLICT (task_id, user_id) DO NOTHING",
        vec![
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(role.to_string()),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 编辑任务（只更新传入的字段）。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删，调用方转 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn update_task(
    ex: &Transaction,
    id: Uuid,
    patch: &TaskPatch,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets: Vec<&'static str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(title) = patch.title.as_ref() {
        sets.push("title = ?");
        params.push(Value::Text(title.clone()));
    }
    if let Some(description) = patch.description.as_ref() {
        sets.push("description = ?");
        params.push(Value::from(description.clone()));
    }
    if let Some(priority) = patch.priority.as_ref() {
        sets.push("priority = ?");
        params.push(Value::Text(priority.clone()));
    }
    if let Some(need_review) = patch.need_review {
        sets.push("need_review = ?");
        params.push(Value::Bool(need_review));
    }
    if let Some((owner_id, dept_id)) = patch.owner.as_ref() {
        sets.push("owner_id = ?::uuid");
        params.push(Value::from(owner_id));
        sets.push("dept_id = ?::uuid");
        params.push(Value::from(dept_id.as_ref()));
    }
    if let Some(parent_id) = patch.parent_id.as_ref() {
        sets.push("parent_id = ?::uuid");
        params.push(Value::from(parent_id.as_ref()));
    }
    if let Some(project_id) = patch.project_id.as_ref() {
        sets.push("project_id = ?::uuid");
        params.push(Value::from(project_id.as_ref()));
    }
    if let Some(start_date) = patch.start_date.as_ref() {
        sets.push("start_date = ?::date");
        params.push(Value::from(*start_date));
    }
    if let Some(due_date) = patch.due_date.as_ref() {
        sets.push("due_date = ?::date");
        params.push(Value::from(*due_date));
    }
    if let Some(estimate_hours) = patch.estimate_hours.as_ref() {
        sets.push("estimate_hours = ?::numeric");
        params.push(Value::from(*estimate_hours));
    }
    // P2-9：挂/换/清单据。`(None, None)` 落两个 NULL —— 与 `ck_biz_task_doc_pair`
    // 的「同时为空」一致，不会出现半对（半对由 `doc_ref::normalize_pair` 在
    // handler 层先拦掉，库侧约束是最后一道）。
    if let Some((doc_type, doc_id)) = patch.doc.as_ref() {
        sets.push("doc_type = ?");
        params.push(Value::from(doc_type.clone()));
        sets.push("doc_id = ?::uuid");
        params.push(Value::from(doc_id.as_ref()));
    }

    if sets.is_empty() {
        return Ok(0);
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));
    params.push(Value::from(&id));

    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
}

/// 软删除任务 (+ 清理它的依赖行)。
///
/// # 为什么这里还要删依赖（软删不触发 CASCADE）
///
/// `biz_task_dependency` 的两个外键都写了 `ON DELETE CASCADE`，但任务是**软删**
/// （`deleted_at`），CASCADE 只在**物理**删除时触发 —— 所以软删会把依赖行留成孤儿
/// （指向一条已删任务）。孤儿不影响读路径（所有依赖查询都 `JOIN biz_task … AND
/// deleted_at IS NULL`），但它会永久累积，且让启动自检（[`crate::shared::startup_guard`]）
/// 报警。
///
/// 依赖关系本身**没有历史价值**（谁依赖谁随时可按业务重建），且该表没有 `deleted_at`
/// 列，所以这里直接 `DELETE`，并与任务软删**同一事务**（要么都成、要么都回滚）。
///
/// # 返回值
///
/// 任务受影响行数（0 = 任务不存在 / 已软删；此时依赖清理照做，重复删除因此幂等）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（事务由调用方回滚）。
pub async fn soft_delete(ex: &Transaction, id: Uuid, operator: Uuid) -> DbResult<u64> {
    let affected = ex
        .execute_with_params(
            "UPDATE biz_task SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&operator), Value::from(&id)],
        )
        .await?;
    purge_dependencies(ex, id).await?;
    Ok(affected)
}

/// 清掉与某个任务相关的**全部**依赖行（双向：我作为后继 + 我作为前置）。
///
/// # 为什么双向
///
/// 只删一侧会留下另一半孤儿：A→B 在库里是一行 `(task_id=A, depends_on_id=B)`，
/// 删 A 要看 `task_id`、删 B 要看 `depends_on_id`。
///
/// # 返回值
///
/// 被删掉的依赖行数（0 = 该任务没有依赖，属正常）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: 软删任务时按 id **双向**清依赖边（DELETE … WHERE task_id = ? OR depends_on_id = ?），写路径、只返回受影响行数；调用 soft_delete（task_model_write.rs:246）← handler.rs:716 delete （:735 require_task 取行 + :737 负责人/创建人/超管 403 判定）。
pub async fn purge_dependencies(ex: &impl SqlExecutor, id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_dependency WHERE task_id = ?::uuid OR depends_on_id = ?::uuid"
            .to_string(),
        vec![Value::from(&id), Value::from(&id)],
    )
    .await
}

/// 落库一次状态流转（**只碰本行**，副作用由调用方另行处理）。
///
/// `ex` 吃 `&impl SqlExecutor`（与 [`recalc_ancestors`] / [`insert_comment`] 同口径）：
/// 状态流转与级联都在调用方的事务里，进不进事务由调用方决定，写状态的实现只有一份。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删）。
///
/// # 异常情况
///
/// 数据库异常（含 `ck_biz_task_status` 违约）→ [`DbError`]。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    update: &StatusUpdate,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets: Vec<&'static str> = vec!["status = ?"];
    let mut params: Vec<Value> = vec![Value::Text(update.to.clone())];

    if let Some(progress) = update.progress {
        sets.push("progress = ?::smallint");
        params.push(Value::I16(progress));
    }
    match update.done_at {
        None => {}
        // 清空与写值分成两条**静态** SQL 片段：`?::timestamptz` 绑 NULL 在驱动层不可靠
        // （nik-db 的参数编码按类型推断，NULL 会被推断成 text），所以清空直接写 `NULL`。
        Some(None) => sets.push("done_at = NULL"),
        Some(Some(stamp)) => {
            sets.push("done_at = ?::timestamptz");
            params.push(Value::from(stamp));
        }
    }
    if let Some(reason) = update.reject_reason.as_ref() {
        sets.push("reject_reason = ?");
        params.push(Value::Text(reason.clone()));
    }
    if let Some(reason) = update.cancel_reason.as_ref() {
        sets.push("cancel_reason = ?");
        params.push(Value::Text(reason.clone()));
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));
    params.push(Value::from(&id));

    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
}

/// 直接改进度（**无子任务的任务才允许**；有子任务的闸在 handler 侧）。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删）。
///
/// # 异常情况
///
/// 数据库异常（含 `ck_biz_task_progress` 越界）→ [`DbError`]。
pub async fn set_progress(
    ex: &Transaction,
    id: Uuid,
    progress: i16,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_task SET progress = ?::smallint, updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::I16(progress),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 写一条任务评论（状态变更 / 进度自动汇总 / AI 报告都走它）。
///
/// `comment_type` 是 `V0108` 新增的列：`PLAIN` / `STATUS_CHANGE` / `PROGRESS_AUTO`
/// （`V0109` 加 `ASSIGN`、`V0112` 加 `AI_REPORT` / `AI_PROGRESS`）。
/// 状态流转的评论**必须**用 `STATUS_CHANGE`（前端详情页的时间线按它画箭头），
/// 否则「谁把任务改成了什么」会和普通闲聊混在一起。
///
/// # 为什么吃 `&impl SqlExecutor` 而不是 `&Transaction`
///
/// T5b 的轮询回传里，评论有三处是「独立写一条」的（失败说明 / 阻塞说明 / 任务丢失），
/// 它们不需要与状态更新同事务；而状态流转那几处仍然要进事务。`SqlExecutor` 同时覆盖
/// `&Db` 与 `&Transaction`（与 `recalc_ancestors` / `add_member` 同一口径），
/// 于是「进不进事务」由调用方决定，写评论的实现只有一份。
///
/// # 返回值
///
/// 新评论 ID（UUIDv7，便于按时间排序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_comment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    user_id: Uuid,
    content: &str,
    comment_type: &str,
    operator: Uuid,
) -> DbResult<Uuid> {
    insert_comment_full(
        ex,
        task_id,
        user_id,
        content,
        comment_type,
        CommentExtra::default(),
        operator,
    )
    .await
}

/// 完整版写评论：额外支持「回复某条评论」与 `@` 名单（T5b 的评论接口用）。
///
/// # 参数
///
/// - `extra.parent_id`：回复的目标评论。数据库外键会保证它存在，
///   但**不校验它属于同一个任务** —— 跨任务回复在 UI 上不会出现（详情页只按
///   `task_id` 取评论），因此这里不做额外查询（少一次往返）。
/// - `extra.mentions`：被 @ 的用户 ID（写进 `mentions` jsonb；空数组写 `'[]'`）。
///
/// 附加项收成 [`CommentExtra`] 而不是两个平铺参数：`clippy` 的
/// `too_many_arguments` 阈值是 7，平铺会直接编译失败（`-D warnings`）。
pub async fn insert_comment_full(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    user_id: Uuid,
    content: &str,
    comment_type: &str,
    extra: CommentExtra<'_>,
    operator: Uuid,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let mentions_json = serde_json::Value::Array(
        extra
            .mentions
            .iter()
            .map(|user_id| serde_json::Value::String(user_id.to_string()))
            .collect(),
    );
    ex.exec_sql(
        "INSERT INTO biz_task_comment \
             (id, task_id, user_id, content, mentions, parent_comment_id, comment_type, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?::jsonb, ?::uuid, ?, now(), now(), ?::uuid, ?::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(content.to_string()),
            Value::Json(mentions_json.to_string()),
            match extra.parent_id {
                Some(parent) => Value::from(&parent),
                None => Value::Null,
            },
            Value::Text(comment_type.to_string()),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 改负责人（分派 / 转派，见 [`ASSIGN_OWNER_SQL`]）。
///
/// # 返回值
///
/// 受影响行数：`0` = 任务已软删 **或** 负责人已被别人改掉（handler 回 409）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn assign_owner(ex: &Transaction, change: &OwnerChange) -> DbResult<u64> {
    ex.execute_with_params(
        ASSIGN_OWNER_SQL,
        vec![
            Value::from(&change.owner_id),
            Value::from(change.dept_id.as_ref()),
            Value::from(change.assigner_id.as_ref()),
            Value::Text(change.source.clone()),
            Value::from(&change.operator),
            Value::from(&change.task_id),
            Value::from(&change.expected_owner),
        ],
    )
    .await
}

/// 批量加协作人（集合式，见 [`MEMBERS_INSERT_SQL`]）。
///
/// # 返回值
///
/// **本次真正新增**的 user_id（幂等命中的不在其中，用于回执里的 `added`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_members(
    ex: &Transaction,
    task_id: Uuid,
    user_ids: &[Uuid],
    operator: Uuid,
) -> DbResult<Vec<Uuid>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            MEMBERS_INSERT_SQL.to_string(),
            vec![
                Value::from(&task_id),
                Value::Text("COLLABORATOR".to_string()),
                Value::from(&operator),
                ids_array_param(user_ids),
            ],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("user_id")?);
    }
    Ok(ids)
}

/// 移除一个协作人。
///
/// # 返回值
///
/// 受影响行数（`0` = 该用户本来就不是协作人 → handler 回 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: 按 (task_id, user_id) 删一行协作关系，写路径、只返回受影响行数，不产出行集；调用 handler.rs:4394（:4375 require_task 取当前行 + :4382 may_manage_members 判定 创建人/负责人/分派人或 business:task:member 码），中间件 business:task:read（handler.rs:4365）。
pub async fn remove_member(ex: &Transaction, task_id: Uuid, user_id: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "DELETE FROM biz_task_member WHERE task_id = ?::uuid AND user_id = ?::uuid",
        vec![Value::from(&task_id), Value::from(&user_id)],
    )
    .await
}

// ==================== 依赖 / 甘特（T4） ====================
