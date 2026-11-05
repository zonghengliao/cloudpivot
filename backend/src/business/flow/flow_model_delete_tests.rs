// ============================================================================
// flow_model_delete_tests.rs —— 真库用例（TASK_DB_URL）：任务删除后审批单自动终止（串行 DB_LOCK）
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::*;
use crate::business::flow::task_bridge;
use crate::business::task::model as task_model;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 夹具操作人 / 审批人（`biz_task.owner_id` / `biz_flow_task.assignee_id` 都是硬 FK）。
const U_ID: &str = "7a9e0000-0000-4000-8000-0000000000c1";
/// 被删除的任务 / 对照组任务 / 无审批单的任务。
const TASK_DEL: &str = "7a9e0000-0000-4000-8000-0000000001a1";
const TASK_OTHER: &str = "7a9e0000-0000-4000-8000-0000000001a2";
const TASK_PLAIN: &str = "7a9e0000-0000-4000-8000-0000000001a3";
const NO_DEL: &str = "T12-DEL";
const NO_OTHER: &str = "T12-OTHER";
const NO_PLAIN: &str = "T12-PLAIN";
/// 流程定义 / 版本（实例的 `def_id` / `version_id` 都是 `ON DELETE RESTRICT`，必须先造）。
const DEF_ID: &str = "7a9e0000-0000-4000-8000-000000000d01";
const VER_ID: &str = "7a9e0000-0000-4000-8000-000000000e01";
/// 实例：删除任务上的 `PENDING` / `APPROVED`，对照组任务上的 `PENDING`。
const INST_PENDING: &str = "7a9e0000-0000-4000-8000-000000000f01";
const INST_APPROVED: &str = "7a9e0000-0000-4000-8000-000000000f02";
const INST_OTHER: &str = "7a9e0000-0000-4000-8000-000000000f03";
/// 实例节点（`biz_flow_task.instance_node_id` 是硬 FK + `uk_biz_flow_task_pending`）。
const NODE_P: &str = "7a9e0000-0000-4000-8000-000000001001";
const NODE_A: &str = "7a9e0000-0000-4000-8000-000000001002";
const NODE_O: &str = "7a9e0000-0000-4000-8000-000000001003";
/// 审批待办。
const FT_P: &str = "7a9e0000-0000-4000-8000-000000001101";
const FT_A: &str = "7a9e0000-0000-4000-8000-000000001102";
const FT_O: &str = "7a9e0000-0000-4000-8000-000000001103";
/// `APPROVED` 实例上原有的审批意见（断言「不动」时要逐字相等）。
const APPROVED_COMMENT: &str = "人工通过，保留审计";

/// 真库用例**串行**执行（共用一个库 + 固定 UUID 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("TASK_DB_URL") else {
                println!("SKIP: 未设置 TASK_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: TASK_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn exec(sql: &str, params: Vec<Value>, what: &str) {
    if let Err(error) = db().execute_with_params(sql, params).await {
        panic!("夹具{what}失败：{error}");
    }
}

async fn insert_task(id: Uuid, task_no: &str, me: Uuid) {
    exec(
        "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, \
                               status, priority, progress, need_review) \
         VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, 'REVIEW', 'P2', 0, true)",
        vec![
            Value::from(&id),
            Value::Text(task_no.to_string()),
            Value::Text(format!("T12 {task_no}")),
            Value::from(&me),
            Value::from(&me),
        ],
        "任务",
    )
    .await;
}

async fn insert_instance(
    id: Uuid,
    instance_no: &str,
    status: &str,
    biz_id: Uuid,
    biz_no: &str,
    me: Uuid,
) {
    // ck_biz_flow_instance_end：`(status='PENDING') = (ended_at IS NULL)`。
    let ended_at = if status == "PENDING" { "NULL" } else { "now()" };
    let result_comment = if status == "APPROVED" {
        format!("'{APPROVED_COMMENT}'")
    } else {
        "NULL".to_string()
    };
    exec(
        &format!(
            "INSERT INTO biz_flow_instance \
                 (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                  biz_type, biz_id, biz_no, title, status, initiator_id, ended_at, result_comment) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1, 't12_delete_flow', 'T12 删除即终止', \
                     'TASK_ACCEPT', ?::uuid, ?, ?::text, ?, ?::uuid, {ended_at}, {result_comment})"
        ),
        vec![
            Value::from(&id),
            Value::Text(instance_no.to_string()),
            Value::from(&uuid(DEF_ID)),
            Value::from(&uuid(VER_ID)),
            Value::from(&biz_id),
            Value::Text(biz_no.to_string()),
            Value::Text(format!("T12 {instance_no}")),
            Value::Text(status.to_string()),
            Value::from(&me),
        ],
        "实例",
    )
    .await;
}

async fn insert_node(id: Uuid, instance_id: Uuid, status: &str) {
    exec(
        "INSERT INTO biz_flow_instance_node \
             (id, instance_id, node_key, node_name, node_type, status) \
         VALUES (?::uuid, ?::uuid, 'n1', '部门经理审批', 'APPROVAL', ?)",
        vec![
            Value::from(&id),
            Value::from(&instance_id),
            Value::Text(status.to_string()),
        ],
        "节点",
    )
    .await;
}

async fn insert_flow_task(id: Uuid, instance_id: Uuid, node_id: Uuid, status: &str, me: Uuid) {
    // ck_biz_flow_task_done：`(status='PENDING') = (completed_at IS NULL)`。
    let completed_at = if status == "PENDING" { "NULL" } else { "now()" };
    exec(
        &format!(
            "INSERT INTO biz_flow_task \
                 (id, instance_id, instance_node_id, node_key, assignee_id, assignee_source, \
                  status, completed_at) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'n1', ?::uuid, 'USER', ?, {completed_at})"
        ),
        vec![
            Value::from(&id),
            Value::from(&instance_id),
            Value::from(&node_id),
            Value::from(&me),
            Value::Text(status.to_string()),
        ],
        "审批待办",
    )
    .await;
}

/// 清掉上一次的夹具（实例 → 版本 → 定义 → 任务；顺序受 FK 约束）。
async fn cleanup(me: Uuid) {
    exec(
        "DELETE FROM biz_flow_instance WHERE def_id = ?::uuid",
        vec![Value::from(&uuid(DEF_ID))],
        "清理实例",
    )
    .await;
    exec(
        "UPDATE biz_flow_def SET current_version_id = NULL WHERE id = ?::uuid",
        vec![Value::from(&uuid(DEF_ID))],
        "清理定义（摘当前版本）",
    )
    .await;
    exec(
        "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid",
        vec![Value::from(&uuid(DEF_ID))],
        "清理版本",
    )
    .await;
    exec(
        "DELETE FROM biz_flow_def WHERE id = ?::uuid",
        vec![Value::from(&uuid(DEF_ID))],
        "清理定义",
    )
    .await;
    // 自引用 FK 不是 deferrable：先断父指针再删（T8-A 实测）。
    exec(
        "UPDATE biz_task SET parent_id = NULL WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
        "清理任务（断父指针）",
    )
    .await;
    exec(
        "DELETE FROM biz_task WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
        "清理任务",
    )
    .await;
}

/// 拿锁 + 连库 + 重建夹具（幂等）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uuid(U_ID);
    exec(
        "INSERT INTO sys_user (id, username, password) \
         VALUES (?::uuid, 't12_flow_user', 't12-not-a-real-hash') ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
        "用户",
    )
    .await;
    cleanup(me).await;
    insert_task(uuid(TASK_DEL), NO_DEL, me).await;
    insert_task(uuid(TASK_OTHER), NO_OTHER, me).await;
    insert_task(uuid(TASK_PLAIN), NO_PLAIN, me).await;
    exec(
        "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status) \
         VALUES (?::uuid, 't12_delete_flow', 'T12 删除即终止', 'TASK_ACCEPT', 'DRAFT')",
        vec![Value::from(&uuid(DEF_ID))],
        "流程定义",
    )
    .await;
    exec(
        "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status) \
         VALUES (?::uuid, ?::uuid, 1, 'DRAFT')",
        vec![Value::from(&uuid(VER_ID)), Value::from(&uuid(DEF_ID))],
        "流程版本",
    )
    .await;
    insert_instance(
        uuid(INST_PENDING),
        "T12-INST-P",
        "PENDING",
        uuid(TASK_DEL),
        NO_DEL,
        me,
    )
    .await;
    insert_instance(
        uuid(INST_APPROVED),
        "T12-INST-A",
        "APPROVED",
        uuid(TASK_DEL),
        NO_DEL,
        me,
    )
    .await;
    insert_instance(
        uuid(INST_OTHER),
        "T12-INST-O",
        "PENDING",
        uuid(TASK_OTHER),
        NO_OTHER,
        me,
    )
    .await;
    insert_node(uuid(NODE_P), uuid(INST_PENDING), "ACTIVE").await;
    insert_node(uuid(NODE_A), uuid(INST_APPROVED), "APPROVED").await;
    insert_node(uuid(NODE_O), uuid(INST_OTHER), "ACTIVE").await;
    insert_flow_task(uuid(FT_P), uuid(INST_PENDING), uuid(NODE_P), "PENDING", me).await;
    insert_flow_task(
        uuid(FT_A),
        uuid(INST_APPROVED),
        uuid(NODE_A),
        "APPROVED",
        me,
    )
    .await;
    insert_flow_task(uuid(FT_O), uuid(INST_OTHER), uuid(NODE_O), "PENDING", me).await;
    Some(guard)
}

/// 与 handler 同款：**一个事务**里「软删任务 + 终止在跑审批单」。
async fn delete_task(task_id: Uuid, task_no: &str, me: Uuid) -> CancelledOfBiz {
    let tx = db().begin().await.expect("开事务失败");
    let affected = task_model::soft_delete(&tx, task_id, me)
        .await
        .expect("软删任务失败");
    assert_eq!(affected, 1, "夹具任务应被软删掉 1 行");
    let out = task_bridge::cancel_on_task_deleted(&tx, task_id, task_no)
        .await
        .expect("终止在跑审批单失败");
    tx.commit().await.expect("提交事务失败");
    out
}

/// 实例三元组：状态 / `ended_at` 是否非空 / `result_comment`。
async fn instance_row(id: Uuid) -> (String, bool, Option<String>) {
    let rows = db()
        .find_with_params(
            "SELECT status, (ended_at IS NOT NULL) AS ended, result_comment \
               FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读实例失败");
    let row = rows.first().expect("实例不存在");
    (
        row.get_typed("status").expect("status"),
        row.get_typed("ended").expect("ended"),
        row.get_typed("result_comment").expect("result_comment"),
    )
}

async fn instance_revision(id: Uuid) -> i32 {
    let rows = db()
        .find_with_params(
            "SELECT revision FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读实例失败");
    rows.first()
        .expect("实例不存在")
        .get_typed("revision")
        .expect("revision")
}

/// 待办三元组：状态 / `completed_at` 是否非空。
async fn flow_task_row(id: Uuid) -> (String, bool) {
    let rows = db()
        .find_with_params(
            "SELECT status, (completed_at IS NOT NULL) AS done FROM biz_flow_task WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读待办失败");
    let row = rows.first().expect("待办不存在");
    (
        row.get_typed("status").expect("status"),
        row.get_typed("done").expect("done"),
    )
}

async fn node_status(id: Uuid) -> String {
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_flow_instance_node WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读节点失败");
    rows.first()
        .expect("节点不存在")
        .get_typed("status")
        .expect("status")
}

async fn task_deleted(id: Uuid) -> bool {
    let rows = db()
        .find_with_params(
            "SELECT (deleted_at IS NOT NULL) AS gone FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读任务失败");
    rows.first()
        .expect("任务不存在")
        .get_typed("gone")
        .expect("gone")
}

/// ① 有 1 个 `PENDING` 实例 → 删任务 → 实例 CANCELLED + `ended_at` 非空 + 文案。
#[tokio::test]
async fn pending_instance_cancelled_with_ended_at_and_comment() {
    let Some(_guard) = setup().await else {
        return;
    };
    let out = delete_task(uuid(TASK_DEL), NO_DEL, uuid(U_ID)).await;
    let (status, ended, comment) = instance_row(uuid(INST_PENDING)).await;
    assert_eq!(status, "CANCELLED", "在跑实例必须被终止");
    assert!(ended, "ended_at 必须非空（ck_biz_flow_instance_end）");
    assert_eq!(
        comment.as_deref(),
        Some(task_bridge::TASK_DELETE_CANCEL_COMMENT),
        "result_comment 必须是删除文案"
    );
    assert_eq!(out.instances, 1, "只该终止 1 个在跑实例");
    assert_eq!(out.tasks, 1, "该实例下的 1 条待办被终止");
    assert_eq!(out.nodes, 1, "该实例下的 1 个活动节点被终止");
    assert!(task_deleted(uuid(TASK_DEL)).await, "任务本身也要被软删");
}

/// ② `APPROVED` 实例 → 删任务 → 实例与它已经审过的待办都**不动**（审计保留）。
#[tokio::test]
async fn approved_instance_untouched() {
    let Some(_guard) = setup().await else {
        return;
    };
    delete_task(uuid(TASK_DEL), NO_DEL, uuid(U_ID)).await;
    let (status, ended, comment) = instance_row(uuid(INST_APPROVED)).await;
    assert_eq!(status, "APPROVED", "已完成的事实不许改");
    assert!(ended, "APPROVED 的 ended_at 本来就非空");
    assert_eq!(
        comment.as_deref(),
        Some(APPROVED_COMMENT),
        "原审批意见不许被覆盖"
    );
    let (task_status, _) = flow_task_row(uuid(FT_A)).await;
    assert_eq!(task_status, "APPROVED", "已通过的票不许改");
    assert_eq!(
        node_status(uuid(NODE_A)).await,
        "APPROVED",
        "已通过的节点不许改"
    );
    // 计数器：只数到那 1 个 PENDING 实例，APPROVED 的没被碰
    let out = delete_task(uuid(TASK_OTHER), NO_OTHER, uuid(U_ID)).await;
    assert_eq!(out.instances, 1, "另一个任务的在跑实例是 1 个");
}

/// ③ `PENDING` 实例 + `PENDING` 待办 → 删任务 → 两者都终止（待办带 `completed_at`）。
#[tokio::test]
async fn pending_flow_task_and_node_cancelled() {
    let Some(_guard) = setup().await else {
        return;
    };
    delete_task(uuid(TASK_DEL), NO_DEL, uuid(U_ID)).await;
    let (status, completed) = flow_task_row(uuid(FT_P)).await;
    assert_eq!(status, "CANCELLED", "审批人的待办里不能再挂着它");
    assert!(completed, "completed_at 必须非空（ck_biz_flow_task_done）");
    assert_eq!(
        node_status(uuid(NODE_P)).await,
        "CANCELLED",
        "活动节点要一起作废"
    );
}

/// ④ 没有审批实例的任务 → 删除正常、计数全 0（不报错）。
#[tokio::test]
async fn task_without_instances_deleted_ok() {
    let Some(_guard) = setup().await else {
        return;
    };
    let tx = db().begin().await.expect("开事务失败");
    let affected = task_model::soft_delete(&tx, uuid(TASK_PLAIN), uuid(U_ID))
        .await
        .expect("软删无审批单的任务不该失败");
    let out = task_bridge::cancel_on_task_deleted(&tx, uuid(TASK_PLAIN), NO_PLAIN)
        .await
        .expect("没有在跑实例时不该报错");
    tx.commit().await.expect("提交事务失败");
    assert_eq!(affected, 1);
    assert_eq!(out, CancelledOfBiz::default(), "三张表都该是 0 行");
    assert!(task_deleted(uuid(TASK_PLAIN)).await);
}

/// ⑤ 事务性：同样的写入放在一个**回滚**的事务里 → 任务没删、实例仍是 PENDING。
#[tokio::test]
async fn rollback_keeps_task_and_pending_instance() {
    let Some(_guard) = setup().await else {
        return;
    };
    let me = uuid(U_ID);
    let tx = db().begin().await.expect("开事务失败");
    task_model::soft_delete(&tx, uuid(TASK_DEL), me)
        .await
        .expect("软删任务失败");
    task_bridge::cancel_on_task_deleted(&tx, uuid(TASK_DEL), NO_DEL)
        .await
        .expect("终止审批单失败");
    tx.rollback().await.expect("回滚失败");
    assert!(!task_deleted(uuid(TASK_DEL)).await, "回滚后任务不该被删");
    let (status, ended, comment) = instance_row(uuid(INST_PENDING)).await;
    assert_eq!(status, "PENDING", "回滚后实例仍是 PENDING");
    assert!(!ended, "回滚后 ended_at 仍为空");
    assert_eq!(comment, None, "回滚后 result_comment 仍为空");
    let (task_status, _) = flow_task_row(uuid(FT_P)).await;
    assert_eq!(task_status, "PENDING", "回滚后待办仍是 PENDING");
}

/// ⑥ 精确匹配：删 A → B 的实例 / 待办 / 节点一个都不许动。
#[tokio::test]
async fn other_biz_instances_untouched() {
    let Some(_guard) = setup().await else {
        return;
    };
    let out = delete_task(uuid(TASK_DEL), NO_DEL, uuid(U_ID)).await;
    assert_eq!(out.instances, 1, "只该动 A 的那 1 个实例");
    let (status, ended, _) = instance_row(uuid(INST_OTHER)).await;
    assert_eq!(status, "PENDING", "别的任务的实例不许动");
    assert!(!ended);
    let (task_status, completed) = flow_task_row(uuid(FT_O)).await;
    assert_eq!(task_status, "PENDING", "别的任务的待办不许动");
    assert!(!completed);
    assert_eq!(
        node_status(uuid(NODE_O)).await,
        "ACTIVE",
        "别的任务的节点不许动"
    );
}

/// ⑦ 幂等：第二遍终止 → 全 0，且不再 bump `revision`。
#[tokio::test]
async fn second_call_is_idempotent() {
    let Some(_guard) = setup().await else {
        return;
    };
    let first = delete_task(uuid(TASK_DEL), NO_DEL, uuid(U_ID)).await;
    assert_eq!(first.instances, 1);
    let revision_after = instance_revision(uuid(INST_PENDING)).await;
    assert_eq!(
        revision_after, 1,
        "终止时 revision + 1（与 bump_instance 同口径）"
    );
    let tx = db().begin().await.expect("开事务失败");
    let second = task_bridge::cancel_on_task_deleted(&tx, uuid(TASK_DEL), NO_DEL)
        .await
        .expect("重复终止不该报错");
    tx.commit().await.expect("提交失败");
    assert_eq!(
        second,
        CancelledOfBiz::default(),
        "已经没有在跑的，三张表都 0 行"
    );
    assert_eq!(
        instance_revision(uuid(INST_PENDING)).await,
        revision_after,
        "重复调用不许再 bump revision"
    );
}

/// ⑧ 文案常量口径（纯逻辑）：验收脚本与单测按同一句断言，不许悄悄改字。
#[test]
fn delete_cancel_comment_is_stable() {
    assert_eq!(
        task_bridge::TASK_DELETE_CANCEL_COMMENT,
        "任务已删除，审批单自动终止"
    );
}
