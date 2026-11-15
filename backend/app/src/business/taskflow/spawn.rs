//! `TASK_NODE` 落库那一段（P2-38 从 `flow/task_node.rs` 搬来）。
//!
//! # 归属
//!
//! 流程引擎决定「**造不造、造什么**」（负责人解析、模板渲染、截止日计算），
//! `biz_task` 的**写入**归本文件 —— 因为写任务表就要 `task::model`，而 `flow` 一旦认识
//! `task` 就与 `task → flow` 成环。本文件在接线域 `business::taskflow`，两边都认识，
//! 是这条依赖唯一合法的落点。
//!
//! # 六条纪律（设计 §7.1，逐条照搬，语义一行未改）
//!
//! 1. **幂等**：先查 `(instance, node_key, round)` 是否已有任务（[`task_of_node`]）；
//!    唯一索引 `uk_biz_task_flow_node` 是最终依据，`23505` 当幂等成功。
//! 2. **失败 → 整笔回滚**：`DbError` 原样上抛，引擎把实例发起一起回滚。
//! 3. **多人解析 → 1 个 owner + 其余挂 `biz_task_member`**（flow 侧已算好 `member_ids`）。
//! 4. **`task_no` 在同一事务内取**（`pg_advisory_xact_lock` 按月串行化）；撞
//!    `uk_biz_task_task_no` 时重试一次（设计 §8.11）。
//! 5. **`source` 固定 `'FLOW'`，`creator = 流程发起人`**（谁发起谁验收）。
//! 6. **写 changelog**：谁、什么时候、因为哪个流程节点创建了这张任务。

use chrono::{NaiveDate, Utc};
use serde_json::json;

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::flow::task_node::{self, FLOW_SOURCE};
use crate::business::flow::task_port::{FlowTaskPort, SpawnOutcome, SpawnTaskSpec};
use crate::business::task::model as task_model;
use cloudpivot_kernel::shared::error_code;

/// 流程任务接缝的实现（`bootstrap::register_flow_task_port` 安装）。
#[derive(Debug, Default)]
pub struct TaskflowSpawnPort;

/// 启动期把本实现注册进 `flow::task_port`。
///
/// # 异常情况
///
/// 重复安装 → `Err`（`bootstrap` 按编程错误处理：prod `exit(1)`、其它 profile `panic`）。
pub fn install() -> std::result::Result<(), String> {
    crate::business::flow::task_port::install(std::sync::Arc::new(TaskflowSpawnPort))
}

#[async_trait]
impl FlowTaskPort for TaskflowSpawnPort {
    fn name(&self) -> &'static str {
        "taskflow_spawn"
    }

    /// 幂等前置查询：同一实例 + 同一节点 + 同一轮是否已有未软删的任务。
    // SCOPE-EXEMPT: 事务内幂等判重（按 flow_instance_id + node_key + round 取已有任务 id，LIMIT 1）；
    // 任务由流程引擎在审批节点上生成，没有请求操作人可注入数据范围；调用方在同一事务 `ex` 上。
    async fn task_of_node(
        &self,
        tx: &Transaction,
        instance_id: Uuid,
        node_key: &str,
        round: i32,
    ) -> DbResult<Option<Uuid>> {
        let rows = tx
            .find_with_params(
                "SELECT id FROM biz_task \
                  WHERE flow_instance_id = ?::uuid AND flow_node_key = ? AND flow_round = ? \
                    AND deleted_at IS NULL \
                  LIMIT 1",
                vec![
                    Value::from(&instance_id),
                    Value::Text(node_key.to_string()),
                    Value::from(round),
                ],
            )
            .await?;
        match rows.first() {
            Some(row) => Ok(Some(row.get_typed("id")?)),
            None => Ok(None),
        }
    }

    async fn spawn_task(&self, tx: &Transaction, spec: SpawnTaskSpec) -> DbResult<SpawnOutcome> {
        // ① 幂等前置查询（并发下由唯一索引兜底；这里只是省掉一次无效写入）
        if let Some(existing) = self
            .task_of_node(tx, spec.instance_id, &spec.node_key, spec.round)
            .await?
        {
            tracing::info!(
                instance_id = %spec.instance_id,
                node_key = %spec.node_key,
                round = spec.round,
                task_id = %existing,
                "任务节点已生成过任务（幂等命中，不发第二条）"
            );
            return Ok(SpawnOutcome::AlreadyExists {
                task_id: Some(existing),
            });
        }

        // ② 负责人的部门快照（看板按 dept_id 分组，与手工建任务同一口径）
        let dept_id = owner_dept(tx, spec.owner_id).await?;

        // ③ 取号 + 落库（撞任务号重试一次）
        let task_id = Uuid::now_v7();
        let month = task_model::month_key(task_node::cst_today(Utc::now()));
        let mut inserted = None;
        let mut last_error: Option<DbError> = None;
        for attempt in 0..2 {
            let task_no = task_model::next_task_no(tx, &month).await?;
            match insert_task(
                tx,
                &FlowTaskInsert {
                    id: task_id,
                    task_no: &task_no,
                    title: &spec.title,
                    description: spec.description.as_deref(),
                    priority: &spec.priority,
                    need_review: spec.need_review,
                    owner_id: spec.owner_id,
                    creator_id: spec.creator_id,
                    dept_id,
                    due_date: spec.due_date,
                    estimate_hours: spec.estimate_hours,
                    flow_instance_id: spec.instance_id,
                    flow_node_key: &spec.node_key,
                    flow_round: spec.round,
                },
            )
            .await
            {
                Ok(()) => {
                    inserted = Some(task_no);
                    break;
                }
                Err(error) if is_flow_node_duplicate(&error) => {
                    // 并发下另一条路径先落了同一行（`uk_biz_task_flow_node`）→ 幂等成功。
                    tracing::info!(
                        instance_id = %spec.instance_id,
                        node_key = %spec.node_key,
                        round = spec.round,
                        "任务已由并发路径生成（23505 当幂等成功）"
                    );
                    return Ok(SpawnOutcome::AlreadyExists {
                        task_id: self
                            .task_of_node(tx, spec.instance_id, &spec.node_key, spec.round)
                            .await?,
                    });
                }
                Err(error) if is_task_no_duplicate(&error) && attempt == 0 => {
                    // §8.11：`next_task_no` 是 MAX+1（虽然已按月加咨询锁，但换月份边界仍有窗口）。
                    tracing::warn!(month = %month, "任务号撞唯一约束，重试一次取号");
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        let Some(task_no) = inserted else {
            // 两次都撞任务号：这是真正的异常（`next_task_no` 有按月咨询锁，只在换月边界有窗口），
            // 原样上抛让整笔事务回滚，而不是吞掉。
            return match last_error {
                Some(error) => Err(error),
                None => Ok(SpawnOutcome::Failed(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    "生成任务编号失败（重试后仍冲突）",
                ))),
            };
        };

        // ④ 协作人（其余解析出来的人）：`biz_task_member` 已存在且 `ON CONFLICT DO NOTHING`。
        let mut member_count = 0usize;
        for candidate in &spec.member_ids {
            task_model::add_member(tx, task_id, *candidate, "COLLABORATOR", spec.creator_id).await?;
            member_count += 1;
        }

        // ⑤ changelog（谁在什么时候因为哪个流程节点创建了这张任务）。
        let after = json!({
            "status": "TODO",
            "progress": 0,
            "owner_id": spec.owner_id.to_string(),
            "source": FLOW_SOURCE,
            "due_date": spec.due_date.map(|date| date.format("%Y-%m-%d").to_string()),
            "flow_instance_id": spec.instance_id.to_string(),
            "flow_node_key": spec.node_key,
            "flow_round": spec.round,
        });
        let record = changelog::record(
            tx,
            &ChangeCtx::api(Some(spec.creator_id), spec.creator_name.as_deref()),
            ChangeTarget::of(
                "biz_task",
                task_id.to_string(),
                Action::Create,
                &format!("流程节点「{}」生成任务", spec.node_name),
            ),
            DiffInput::create(&after),
        )
        .await;
        if let Err(error) = record {
            // 审计写不进去就让整笔回滚（与 `changelog::record` 的设计立场一致：
            // 业务回滚而日志留下的「假日志」比没有日志更糟）。
            return Ok(SpawnOutcome::Failed(error));
        }

        tracing::info!(
            instance_id = %spec.instance_id,
            node_key = %spec.node_key,
            round = spec.round,
            task_id = %task_id,
            task_no = %task_no,
            owner_id = %spec.owner_id,
            members = member_count,
            due_date = ?spec.due_date,
            "任务节点已生成工作任务"
        );
        Ok(SpawnOutcome::Created {
            task_id,
            task_no,
            owner_id: spec.owner_id,
            members: member_count,
            due_date: spec.due_date,
        })
    }

    /// 审批结论落地：先推进任务状态（本域），再推进流程节点（`flow::engine`）。
    ///
    /// 逐字等价于 P2-38 之前 `flow::task_bridge::on_instance_settled` 的实现 ——
    /// 只是换了归属域，事务边界与「失败只 WARN」的口径一行没动。
    async fn on_instance_settled(&self, instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
        crate::business::taskflow::bridge::on_instance_settled(instance_id, instance_status).await
    }
}

/// 负责人的部门快照（任务看板按 `dept_id` 分组，与手工建任务同一口径）。
// SCOPE-EXEMPT: 按 user_id 单条取负责人部门快照（写入 biz_task.dept_id 供看板分组），返回单列、
// 不产出行集；输入是流程已选定的负责人；调用方在流程引擎事务内，无操作人。
async fn owner_dept(tx: &Transaction, owner_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = tx
        .find_with_params(
            "SELECT dept_id FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&owner_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(row.get_typed::<Option<Uuid>>("dept_id").ok().flatten()),
        None => Ok(None),
    }
}

/// 流程任务的落库入参（字段集中在一处，避免 `too_many_arguments`）。
struct FlowTaskInsert<'a> {
    id: Uuid,
    task_no: &'a str,
    title: &'a str,
    description: Option<&'a str>,
    priority: &'a str,
    need_review: bool,
    owner_id: Uuid,
    creator_id: Uuid,
    dept_id: Option<Uuid>,
    due_date: Option<NaiveDate>,
    estimate_hours: Option<f64>,
    flow_instance_id: Uuid,
    flow_node_key: &'a str,
    flow_round: i32,
}

/// 落一行 `biz_task`（**含 `flow_*` 三列**；`task::model::create_task` 不含这三列，
/// 而设计红线要求 `biz_task` 的流程侧写入全部收在本文件）。
async fn insert_task(tx: &Transaction, row: &FlowTaskInsert<'_>) -> DbResult<()> {
    tx.execute_with_params(
        "INSERT INTO biz_task \
                 (id, task_no, title, description, status, priority, progress, need_review, \
                  owner_id, creator_id, assigner_id, dept_id, parent_id, project_id, \
                  start_date, due_date, done_at, estimate_hours, actual_hours, source, \
                  cancel_reason, reject_reason, approval_instance_id, \
                  created_at, updated_at, create_by, update_by, \
                  flow_instance_id, flow_node_key, flow_round) \
             VALUES (?::uuid, ?, ?, ?, 'TODO', ?, 0, ?, \
                     ?::uuid, ?::uuid, NULL, ?::uuid, NULL, NULL, \
                     NULL, ?::date, NULL, ?::numeric, NULL, ?, \
                     NULL, NULL, NULL, \
                     now(), now(), ?::uuid, ?::uuid, \
                     ?::uuid, ?, ?)",
        vec![
            Value::from(&row.id),
            Value::Text(row.task_no.to_string()),
            Value::Text(row.title.to_string()),
            Value::from(row.description),
            Value::Text(row.priority.to_string()),
            Value::Bool(row.need_review),
            Value::from(&row.owner_id),
            Value::from(&row.creator_id),
            Value::from(row.dept_id.as_ref()),
            Value::from(row.due_date),
            Value::from(row.estimate_hours),
            Value::Text(FLOW_SOURCE.to_string()),
            Value::from(&row.creator_id),
            Value::from(&row.creator_id),
            Value::from(&row.flow_instance_id),
            Value::Text(row.flow_node_key.to_string()),
            Value::from(row.flow_round),
        ],
    )
    .await?;
    Ok(())
}

/// 是否「同一实例同一节点同一轮」的唯一约束冲突（`uk_biz_task_flow_node`）。
///
/// `pub`：集成用例 `tests/pflow1a.rs` 用它断言「重复生成被唯一索引挡下」。
pub fn is_flow_node_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_task_flow_node"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_task_flow_node"),
    }
}

/// 是否任务号唯一约束冲突（`uk_biz_task_task_no`）。
fn is_task_no_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_task_task_no"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_task_task_no"),
    }
}
