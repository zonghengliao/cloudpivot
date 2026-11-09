// ============================================================================
// task_model_schedule.rs —— 改期：排期快照读取与落库
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_base::*;
use super::task_model_rows::*;

/// 事务内读本任务的排期快照（不存在 / 已软删 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: 事务内按 task_id 读本任务的排期快照（单行），不产出行集；唯一调用方 business/task/schedule.rs:545，其前置已做可见性 find_detail（schedule.rs:517）与「负责人/创建人/超管」写权限判定（schedule.rs:522）。
pub async fn find_schedule_row(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ScheduleRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, task_no, title, status, start_date, due_date, created_at, parent_id \
               FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(ScheduleRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 未完成 / 未取消的前置（FS 边，「我依赖谁」）。
///
/// SQL 只做「关系筛选」（`d.task_id = 我` + `dep_type = 'FS'` + 对端未软删），
/// **不在这里过滤状态**：`DONE` / `CANCELLED` 是否算「已解除」的唯一判据是
/// [`crate::business::task::dep::is_blocking_status`]（纯逻辑，可单测）。
// SCOPE-EXEMPT: 按 task_id 取「我依赖谁」的 FS 边（校验用的关系集合）；调用方 schedule.rs:566。注入 scope 会把看不见的前置过滤掉 → 改期校验静默漏判（比越权更难发现），可见性/写权限已在 schedule.rs:517 / :524 判定。
pub async fn schedule_prerequisites(ex: &impl SqlExecutor, task_id: Uuid) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.depends_on_id AND p.deleted_at IS NULL \
                  WHERE d.task_id = ?::uuid AND d.dep_type = 'FS' \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 后继（谁依赖我；`depends_on_id = 我`）。
// SCOPE-EXEMPT: 按 task_id 取「谁依赖我」的 FS 边（同上，校验用关系集合）；调用方 schedule.rs:567，注入 scope 会让看不见的后继被漏判；权限点 schedule.rs:517 / :524。
pub async fn schedule_successors(ex: &impl SqlExecutor, task_id: Uuid) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.task_id AND p.deleted_at IS NULL \
                  WHERE d.depends_on_id = ?::uuid AND d.dep_type = 'FS' \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 父任务的排期（本任务没有父任务 / 父任务已软删 → `None`）。
// SCOPE-EXEMPT: 按 parent_id 取父任务排期单行（父子区间校验用）；调用方 schedule.rs:569，权限点 schedule.rs:517 / :524。
pub async fn schedule_parent(ex: &impl SqlExecutor, parent_id: Uuid) -> DbResult<Option<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task p \
                  WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
            ),
            vec![Value::from(&parent_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(PeerSchedule::from_row(row)?)),
        None => Ok(None),
    }
}

/// 子任务的排期（本卡**不级联**：父任务改期时用它做「别把子任务甩出去」的校验）。
// SCOPE-EXEMPT: 按 task_id 取子任务排期集合（「别把子任务甩出父区间」校验用）；调用方 schedule.rs:572，权限点 schedule.rs:517 / :524；产出行集仅用于校验，不返回给前端。
pub async fn schedule_children(ex: &impl SqlExecutor, task_id: Uuid) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task p \
                  WHERE p.parent_id = ?::uuid AND p.deleted_at IS NULL \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 落库新的起止日（只写这两列 + 更新时间）。
///
/// # 为什么复用 `TaskPatch` 的绑法而不是自己拼
///
/// `start_date = ?::date` 配 `Value::from(Option<NaiveDate>)` 是编辑接口已经验证
/// 过的组合（`Value::Text` 绑 `?::date` 会在驱动层报
/// `error serializing parameter`，T6b 踩过）。这里只写两列，`sets` 固定非空，
/// 不走 `update_task` 的「字段缺席」分支 —— 改期一定会改至少一个日期。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删，调用方转 404）。
pub async fn update_schedule(
    ex: &Transaction,
    id: Uuid,
    start_date: Option<NaiveDate>,
    due_date: Option<NaiveDate>,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task SET start_date = ?::date, due_date = ?::date, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::from(start_date),
            Value::from(due_date),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}
