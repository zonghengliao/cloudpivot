//! P-FLOW-1B 端到端验收（**真库**：`cloudpivot_test_pflow1b` 或库名含 `pflow1b`/`cloudpivot_test`）。
//!
//! 设计依据：`docs/architecture/流程全配置化-设计.md` §3.4 / §4.5 Q2 / §7.2。
//!
//! | 用例 | 卡面验收 | 断言（**两侧状态**） |
//! |---|---|---|
//! | `complete_without_review_advances_flow` | ③ 场景 1 | 任务 `DONE`；节点 `APPROVED`；下游节点 `ACTIVE` + 新任务 `TODO`；实例 `PENDING` |
//! | `review_pass_approves_node` | ③ 场景 2 | 提交验收：任务 `REVIEW` / 节点仍 `ACTIVE`；验收通过：任务 `DONE` / 节点 `APPROVED` / 实例 `APPROVED` |
//! | `reject_stay_keeps_node_active` | ③ 场景 3 | 打回：任务 `DOING` / 节点仍 `ACTIVE` / 实例 `PENDING` |
//! | `reject_back_to_prev_reopens_previous` | ③ 场景 4 | 打回：任务 `DOING` / 本节点 `REJECTED` / 上一节点第 2 轮 `ACTIVE` + 新任务 |
//! | `cancel_task_terminates_instance` | ③ 场景 5 | 任务 `CANCELLED` / 实例 `CANCELLED` |
//! | `withdraw_instance_cancels_unfinished_tasks` | ③ 场景 6 | 实例 `WITHDRAWN` / 任务 `CANCELLED` |
//! | `approve_mode_full_chain` | ④ | 任务 `DONE` → `approval_instance_id` 非空 → 审批通过 → 节点 `APPROVED` → 下游驱动 |
//! | `approve_mode_reject_follows_on_reject` | ④ 驳回分支 | 驳回 → 节点按 `on_reject=BACK_TO_PREV` 退回上一轮 |
//! | `conflict_when_instance_already_cancelled` | ⑤ | `409 + 65038`，**两侧状态一个字节都不变**（事务干净） |
//! | `concurrent_complete_only_one_advances` | ⑥ | 一次成功、一次 `409 + 65020`；节点只通过一次 |
//! | `flow_task_outcome_four_branches` | ② 补强 | 4 个分支（`NotFlowTask`/`Advanced`/`Ignored`/`Conflict`）在真库各命中一次 |
//! | `plain_task_is_untouched` | ⑦ | 普通任务（`source != 'FLOW'`）的 `change_status` 行为不变 |
//!
//! # 连库约定
//!
//! 只连 `PFLOWB_DB_URL` 指向的库，且**库名必须含 `pflow1b` 或 `cloudpivot_test`**（本套会造真实
//! 流程实例与任务）。跑法：
//!
//! ```bash
//! PFLOWB_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_pflow1b' \
//!   cargo test --test pflow1b -- --test-threads=1 --nocapture
//! ```

use axum::Json;
use axum::extract::Path;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::flow::graph::{
    self, AssigneeKind, EdgeSpec, FlowGraph, Issue, NodeSpec, NodeType,
};
use cloudpivot::business::flow::task_bridge::{self, FlowTaskOutcome, TaskStatusChanged};
use cloudpivot::business::flow::{dto as flow_dto, handler as flow_handler, model as flow_model};
use cloudpivot::business::task::state::TaskStatus;
use cloudpivot::business::task::{dto as task_dto, handler as task_handler};
use cloudpivot::shared::error_code;

// ==================== 连库 ====================

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("PFLOWB_DB_URL") else {
                println!("SKIP: 未设置 PFLOWB_DB_URL（P-FLOW-1B 端到端验收需要真库）");
                return false;
            };
            assert!(
                url.contains("pflow1b") || url.contains("/cloudpivot_test"),
                "PFLOWB_DB_URL 必须指向 cloudpivot_test_pflow1b 或 cloudpivot_test（当前：{url}）"
            );
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => {
                        println!("[pflow1b] 已连接 {url}");
                        true
                    }
                    Err(error) => panic!("set_db 失败：{error}"),
                },
                Err(error) => panic!("连接 {url} 失败：{error}"),
            }
        })
        .await
}

async fn ready() -> Option<()> {
    if ensure_db().await { Some(()) } else { None }
}

fn claims(user: Uuid, name: &str) -> Extension<AuthClaims> {
    Extension(AuthClaims(TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(name.to_string()),
        roles: vec!["SUPER_ADMIN".to_string()],
        // `business:flow:admin` 是「驳回并终止」的必需权限（场景 8 要用）
        permissions: vec!["business:flow:admin".to_string()],
        token_type: Some(TokenType::Access),
        exp: 4_000_000_000,
        iat: 1_700_000_000,
        extra: std::collections::HashMap::new(),
    }))
}

// ==================== 夹具 ====================

/// 一次用例的夹具：一个操作人（流程发起人 + 任务负责人 + 验收人 + 审批人）。
///
/// 为什么只养一个人：`assignee_type=INITIATOR` 让「发起人 = 负责人」，而
/// `task_node::spawn` 把 `creator` 也置为流程发起人 ⇒ 同一个人同时具备
/// `OWNER` / `CREATOR` / `ASSIGNER`，能把 5 态里的所有人工边都推一遍
/// （`REVIEW→DONE` 要 `CREATOR`、`REVIEW→DOING` 要 `CREATOR`+原因）。
struct Fixture {
    tag: String,
    operator: Uuid,
}

impl Fixture {
    async fn new(tag: &str) -> Self {
        let operator = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
             VALUES (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN')",
            vec![
                Value::from(&operator),
                Value::Text(format!(
                    "pflowb-{tag}-{}",
                    &Uuid::now_v7().simple().to_string()[24..32]
                )),
                Value::Text(format!("PFLOWB {tag} 操作人")),
            ],
        )
        .await
        .expect("建夹具用户");
        Self {
            tag: tag.to_string(),
            operator,
        }
    }

    fn ext(&self) -> Extension<AuthClaims> {
        claims(self.operator, "pflowb-op")
    }

    /// 清掉本用例造的一切（按流程编码前缀 `pflowb_<tag>_`）。
    async fn cleanup(&self) {
        let prefix = format!("pflowb_{}_", self.tag.to_ascii_lowercase());
        let like = Value::Text(format!("{prefix}%"));
        for sql in [
            "DELETE FROM biz_task WHERE flow_instance_id IN ( \
                 SELECT id FROM biz_flow_instance WHERE flow_code LIKE ? )",
            "DELETE FROM biz_flow_instance WHERE flow_code LIKE ?",
            "DELETE FROM biz_flow_node WHERE version_id IN ( \
                 SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                  WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_edge WHERE version_id IN ( \
                 SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                  WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_def WHERE flow_code LIKE ?",
        ] {
            let _ = db().execute_with_params(sql, vec![like.clone()]).await;
        }
        // 任务（含 APPROVE 模式发起审批单时的 `biz_task.approval_instance_id` 反查）
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_task WHERE owner_id = ?::uuid",
                vec![Value::from(&self.operator)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user WHERE id = ?::uuid",
                vec![Value::from(&self.operator)],
            )
            .await;
    }
}

// ==================== 建图 / 建流程 / 发起 ====================

/// 一份任务节点的 `config.task`。
fn task_cfg(need_review: bool, on_reject: &str, on_cancel: &str) -> serde_json::Value {
    serde_json::json!({
        "title_template": "{{instance.biz_no}} 处理",
        "need_review": need_review,
        "on_complete": "DIRECT",
        "on_reject": on_reject,
        "on_cancel": on_cancel,
    })
}

/// `APPROVE` 模式的任务节点配置。
fn approve_task_cfg(on_reject: &str, approve_biz_type: &str) -> serde_json::Value {
    serde_json::json!({
        "title_template": "{{instance.biz_no}} 处理",
        "need_review": false,
        "on_complete": "APPROVE",
        "approve_biz_type": approve_biz_type,
        "on_reject": on_reject,
        "on_cancel": "TERMINATE",
    })
}

/// 造一个任务节点（指派来源 = 发起人；`empty_assignee=ERROR` 不许静默丢活）。
fn task_node(key: &str, name: &str, cfg: serde_json::Value) -> NodeSpec {
    let mut node = NodeSpec::new(key, name, NodeType::Task);
    node.assignee_type = Some(AssigneeKind::Initiator);
    node.empty_assignee = "ERROR".to_string();
    node.timeout_action = "REMIND".to_string();
    node.config = serde_json::json!({ "task": cfg });
    node
}

/// `START → TASK_NODE → END`。
fn single_task_graph(cfg: serde_json::Value) -> FlowGraph {
    FlowGraph {
        nodes: vec![
            NodeSpec::new("n_start", "开始", NodeType::Start),
            task_node("n_task", "处理", cfg),
            NodeSpec::new("n_end", "结束", NodeType::End),
        ],
        edges: vec![
            EdgeSpec::new("e1", "n_start", "n_task"),
            EdgeSpec::new("e2", "n_task", "n_end"),
        ],
    }
}

/// `START → n_a(TASK) → n_b(TASK) → END`（用来验「驱动下游」与「退回上一节点」）。
fn chain_task_graph(a: serde_json::Value, b: serde_json::Value) -> FlowGraph {
    FlowGraph {
        nodes: vec![
            NodeSpec::new("n_start", "开始", NodeType::Start),
            task_node("n_a", "第一关", a),
            task_node("n_b", "第二关", b),
            NodeSpec::new("n_end", "结束", NodeType::End),
        ],
        edges: vec![
            EdgeSpec::new("e1", "n_start", "n_a"),
            EdgeSpec::new("e2", "n_a", "n_b"),
            EdgeSpec::new("e3", "n_b", "n_end"),
        ],
    }
}

/// `START → APPROVAL(发起人自审允许) → END`：给 `on_complete=APPROVE` 当被调用的审批流。
fn approval_graph() -> FlowGraph {
    let mut approve = NodeSpec::new("n_approve", "签字", NodeType::Approval);
    approve.assignee_type = Some(AssigneeKind::Initiator);
    // 自审必须允许：本夹具只有一个用户，SKIP 会解析出 0 人 → 65018
    approve.self_approval = "ALLOW".to_string();
    approve.empty_assignee = "ERROR".to_string();
    approve.timeout_action = "REMIND".to_string();
    FlowGraph {
        nodes: vec![
            NodeSpec::new("n_start", "开始", NodeType::Start),
            approve,
            NodeSpec::new("n_end", "结束", NodeType::End),
        ],
        edges: vec![
            EdgeSpec::new("e1", "n_start", "n_approve"),
            EdgeSpec::new("e2", "n_approve", "n_end"),
        ],
    }
}

fn graph_to_save_req(graph: &FlowGraph) -> flow_dto::GraphSaveReq {
    flow_dto::GraphSaveReq {
        nodes: graph
            .nodes
            .iter()
            .map(|node| flow_dto::GraphNodeReq {
                key: node.key.clone(),
                name: node.name.clone(),
                node_type: node.node_type.as_str().to_string(),
                x: None,
                y: None,
                sort_order: None,
                assignee_type: node.assignee_type.map(|kind| kind.as_str().to_string()),
                superior_levels: None,
                form_var_key: None,
                approval_mode: None,
                approve_ratio: None,
                reject_veto: None,
                empty_assignee: Some(node.empty_assignee.clone()),
                self_approval: Some(node.self_approval.clone()),
                allow_robot: None,
                join_mode: None,
                fork_mode: None,
                timeout_hours: None,
                timeout_action: Some(node.timeout_action.clone()),
                remind_before_hours: None,
                max_remind_times: None,
                cc_on_approved: None,
                config: Some(node.config.clone()),
                assignees: Vec::new(),
            })
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|edge| flow_dto::GraphEdgeReq {
                key: edge.key.clone(),
                from: edge.from.clone(),
                to: edge.to.clone(),
                condition: None,
                label: None,
                priority: None,
                is_default: None,
            })
            .collect(),
        change_note: Some("P-FLOW-1B 端到端".to_string()),
    }
}

/// 建流程 + 存图 + 发布，返回 `def_id`。
async fn create_and_publish(
    f: &Fixture,
    tag: &str,
    graph: &FlowGraph,
    biz_type: &str,
    is_default: bool,
) -> Uuid {
    let flow_code = format!(
        "pflowb_{}_{}",
        tag.to_ascii_lowercase(),
        &Uuid::now_v7().simple().to_string()[24..32]
    );
    let create = flow_dto::DefCreateReq {
        flow_code,
        flow_name: format!("PFLOWB {tag}"),
        biz_type: biz_type.to_string(),
        category: None,
        description: None,
        is_default: Some(is_default),
        allow_auto_approve: Some(false),
    };
    let res = flow_handler::create(f.ext(), Json(create))
        .await
        .unwrap_or_else(|error| panic!("建流程失败：{} {}", error.code, error.msg));
    let data = res.data.expect("建流程必须返回 DefSaveRes");
    let def_id = Uuid::parse_str(&data.id).expect("def id");
    let version_id = Uuid::parse_str(&data.version_id).expect("version id");

    flow_handler::save_graph(
        f.ext(),
        Path(version_id.to_string()),
        Json(graph_to_save_req(graph)),
    )
    .await
    .unwrap_or_else(|error| panic!("保存整图失败：{} {}", error.code, error.msg));
    flow_handler::publish(f.ext(), Path(def_id.to_string()))
        .await
        .unwrap_or_else(|error| panic!("发布失败：{} {}", error.code, error.msg));
    def_id
}

/// 发起实例，返回 instance_id。
async fn start_instance(f: &Fixture, def_id: Uuid, title: &str) -> Uuid {
    let req = flow_dto::InstanceCreateReq {
        def_id: Some(def_id.to_string()),
        flow_code: None,
        biz_type: None,
        biz_id: None,
        biz_no: Some(format!(
            "PFLOWB-{}",
            &Uuid::now_v7().simple().to_string()[24..32]
        )),
        title: title.to_string(),
        form_data: Some(serde_json::json!({})),
    };
    let res = flow_handler::create_instance(f.ext(), Json(req))
        .await
        .unwrap_or_else(|error| panic!("发起实例失败：{} {}", error.code, error.msg));
    let data = res.data.expect("发起必须返回 ActionRes");
    Uuid::parse_str(&data.instance_id).expect("instance id")
}

// ==================== 读状态 ====================

/// 某实例某节点某轮的状态（没有该行时 `None`）。
async fn node_status(instance_id: Uuid, key: &str, round: i32) -> Option<String> {
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND node_key = ? AND round = ?",
            vec![
                Value::from(&instance_id),
                Value::Text(key.to_string()),
                Value::from(round),
            ],
        )
        .await
        .expect("查节点状态");
    rows.first().map(|row| row.get_typed("status").unwrap())
}

async fn instance_row(instance_id: Uuid) -> (String, i32) {
    let rows = db()
        .find_with_params(
            "SELECT status, round_total FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例");
    let row = rows.first().expect("实例必须存在");
    (
        row.get_typed("status").unwrap(),
        row.get_typed("round_total").unwrap(),
    )
}

/// 某实例某节点某轮生成的任务（`(id, status, cancel_reason)`）。
async fn flow_task_of(
    instance_id: Uuid,
    key: &str,
    round: i32,
) -> Option<(Uuid, String, Option<String>)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status, cancel_reason FROM biz_task \
              WHERE flow_instance_id = ?::uuid AND flow_node_key = ? AND flow_round = ? \
                AND deleted_at IS NULL",
            vec![
                Value::from(&instance_id),
                Value::Text(key.to_string()),
                Value::from(round),
            ],
        )
        .await
        .expect("查流程任务");
    rows.first().map(|row| {
        (
            row.get_typed("id").unwrap(),
            row.get_typed("status").unwrap(),
            row.get_typed("cancel_reason").unwrap(),
        )
    })
}

async fn task_status(task_id: Uuid) -> String {
    let rows = db()
        .find_with_params(
            "SELECT status, approval_instance_id FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await
        .expect("查任务");
    rows.first()
        .expect("任务必须存在")
        .get_typed("status")
        .unwrap()
}

async fn task_approval_instance(task_id: Uuid) -> Option<Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT approval_instance_id FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await
        .expect("查任务");
    rows.first()
        .expect("任务必须存在")
        .get_typed("approval_instance_id")
        .unwrap()
}

// ==================== 推状态 ====================

async fn change_task(
    f: &Fixture,
    task_id: Uuid,
    to: TaskStatus,
    reason: Option<&str>,
) -> std::result::Result<String, ApiError> {
    let req = task_dto::TaskStatusChangeReq {
        to: to.code().to_string(),
        reason: reason.map(str::to_string),
    };
    let res = task_handler::change_status(f.ext(), Path(task_id.to_string()), Json(req)).await?;
    Ok(res
        .data
        .expect("change_status 必须返回 TaskStatusRes")
        .status)
}

/// 一路推到 `DONE`（`TODO→DOING→DONE`，`need_review=false` 的路径）。
async fn drive_to_done(f: &Fixture, task_id: Uuid) {
    change_task(f, task_id, TaskStatus::Todo, None).await.ok();
    change_task(f, task_id, TaskStatus::Doing, None)
        .await
        .unwrap_or_else(|e| panic!("开始失败：{} {}", e.code, e.msg));
    change_task(f, task_id, TaskStatus::Done, None)
        .await
        .unwrap_or_else(|e| panic!("完成失败：{} {}", e.code, e.msg));
}

/// 审批单里第一条 `PENDING` 待办的 id。
async fn pending_flow_task(instance_id: Uuid) -> Uuid {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY created_at LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查审批待办");
    rows.first()
        .expect("审批单必须有待办")
        .get_typed("id")
        .unwrap()
}

// ==================== 验收审批流的开/关（人工验收路径的前置） ====================

/// 暂时停用 seed 里的 `TASK_ACCEPT` 验收审批流，返回被停用的 def id（**用完必须恢复**）。
///
/// # 为什么需要这一步
///
/// 本库 seed 里有一条已发布的 `task_accept`（`biz_type = TASK_ACCEPT`，
/// `20000000-0000-4000-8000-00000000d127`）。D3b 的既有行为是「提交验收即发起审批」：
/// `DOING→REVIEW` 会自动建一张验收审批单，随后 `REVIEW` 的两条出边被门闸挡成
/// `409 + 65030`（等审批结论）。那不是本卡引入的缺陷，而是**产品语义**。
///
/// 要验「人工验收 / 人工打回」这条纯状态机路径，就得让库先回到「没配验收流程」的状态
/// （= `prepare_accept` 返回 `None`，任务照原两级验收走）。用完立刻恢复，避免污染别的套件。
///
/// 「提交验收 → 审批通过 → 任务 DONE → 节点 APPROVED」那条路另有用例
/// （`task_accept_approval_advances_node`，不关这条流程）。
async fn suspend_task_accept_flow() -> Vec<Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_flow_def \
              WHERE biz_type = 'TASK_ACCEPT' AND status = 'PUBLISHED' AND deleted_at IS NULL",
            Vec::new(),
        )
        .await
        .expect("查 TASK_ACCEPT 流程");
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.get_typed::<Uuid>("id").unwrap())
        .collect();
    if !ids.is_empty() {
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'DISABLED', updated_at = now() \
              WHERE biz_type = 'TASK_ACCEPT' AND status = 'PUBLISHED' AND deleted_at IS NULL",
            Vec::new(),
        )
        .await
        .expect("停用 TASK_ACCEPT 流程");
    }
    ids
}

/// 恢复 [`suspend_task_accept_flow`] 停用的流程。
async fn restore_task_accept_flow(ids: &[Uuid]) {
    for id in ids {
        let _ = db()
            .execute_with_params(
                "UPDATE biz_flow_def SET status = 'PUBLISHED', updated_at = now() WHERE id = ?::uuid",
                vec![Value::from(id)],
            )
            .await;
    }
}

// ==================== ③ 场景 1：need_review=false 点完成 ====================

#[tokio::test]
async fn complete_without_review_advances_flow() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s1").await;
    let graph = chain_task_graph(
        task_cfg(false, "STAY", "TERMINATE"),
        task_cfg(false, "STAY", "TERMINATE"),
    );
    let def_id = create_and_publish(&f, "s1", &graph, "PFLOWB_TASK", false).await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景1").await;

    // 发起后：A 节点 ACTIVE + 任务 TODO；B 节点还不存在（进入即生成）
    let (task_a, status_a, _) = flow_task_of(instance_id, "n_a", 1)
        .await
        .expect("节点 A 必须生成任务");
    assert_eq!(status_a, "TODO", "① 发起后任务 A 是 TODO");
    assert_eq!(
        node_status(instance_id, "n_a", 1).await.as_deref(),
        Some("ACTIVE")
    );
    assert!(
        node_status(instance_id, "n_b", 1).await.is_none(),
        "① 下游还没到，不该有节点行"
    );

    // 完成任务 A
    drive_to_done(&f, task_a).await;

    // **两侧状态**：任务 DONE / 节点 A APPROVED / 下游驱动出 B（节点 ACTIVE + 任务 TODO）
    assert_eq!(task_status(task_a).await, "DONE", "② 任务 A 必须 DONE");
    assert_eq!(
        node_status(instance_id, "n_a", 1).await.as_deref(),
        Some("APPROVED"),
        "② 节点 A 必须 ACTIVE→APPROVED"
    );
    assert_eq!(
        node_status(instance_id, "n_b", 1).await.as_deref(),
        Some("ACTIVE"),
        "② 下游节点 B 必须被驱动为 ACTIVE"
    );
    let (_, status_b, _) = flow_task_of(instance_id, "n_b", 1)
        .await
        .expect("② 下游节点 B 必须生成任务");
    assert_eq!(status_b, "TODO", "② 下游任务 B 是 TODO 等人干");
    assert_eq!(
        instance_row(instance_id).await.0,
        "PENDING",
        "② 实例仍 PENDING"
    );
    println!("[PASS] 场景1：完成任务 → 节点 APPROVED → 下游驱动（两侧状态一致）");
    f.cleanup().await;
}

// ==================== ③ 场景 2：need_review=true 提交验收 → 通过 ====================

#[tokio::test]
async fn review_pass_approves_node() {
    if ready().await.is_none() {
        return;
    }
    let suspended = suspend_task_accept_flow().await;
    let f = Fixture::new("s2").await;
    let def_id = create_and_publish(
        &f,
        "s2",
        &single_task_graph(task_cfg(true, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景2").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1)
        .await
        .expect("必须生成任务");

    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    let status = change_task(&f, task_id, TaskStatus::Review, None)
        .await
        .expect("提交验收");
    assert_eq!(status, "REVIEW", "① 提交验收后任务 REVIEW");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("ACTIVE"),
        "① 提交验收**不动**节点（等验收）"
    );

    let status = change_task(&f, task_id, TaskStatus::Done, None)
        .await
        .expect("验收通过");
    assert_eq!(status, "DONE", "② 验收通过后任务 DONE");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("APPROVED"),
        "② 节点必须 ACTIVE→APPROVED"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "APPROVED",
        "② 这是最后一个节点 → 实例 APPROVED"
    );
    println!("[PASS] 场景2：提交验收不动节点 / 验收通过才推节点（两侧状态一致）");
    f.cleanup().await;
    restore_task_accept_flow(&suspended).await;
}

// ==================== ③ 场景 3：打回 on_reject=STAY ====================

#[tokio::test]
async fn reject_stay_keeps_node_active() {
    if ready().await.is_none() {
        return;
    }
    let suspended = suspend_task_accept_flow().await;
    let f = Fixture::new("s3").await;
    let def_id = create_and_publish(
        &f,
        "s3",
        &single_task_graph(task_cfg(true, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景3").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1)
        .await
        .expect("必须生成任务");

    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    change_task(&f, task_id, TaskStatus::Review, None)
        .await
        .expect("提交验收");
    let status = change_task(&f, task_id, TaskStatus::Doing, Some("数据不对，请重新核对"))
        .await
        .expect("打回");
    assert_eq!(status, "DOING", "① 打回后任务 DOING");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("ACTIVE"),
        "① on_reject=STAY：节点保持 ACTIVE 等返工"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "PENDING",
        "① 实例仍 PENDING、轮次不变"
    );
    assert_eq!(instance_row(instance_id).await.1, 1, "① STAY 不涨轮次");
    println!("[PASS] 场景3：on_reject=STAY 打回 → 节点/实例都不动");
    f.cleanup().await;
    restore_task_accept_flow(&suspended).await;
}

// ==================== ③ 场景 4：打回 on_reject=BACK_TO_PREV ====================

#[tokio::test]
async fn reject_back_to_prev_reopens_previous() {
    if ready().await.is_none() {
        return;
    }
    let suspended = suspend_task_accept_flow().await;
    let f = Fixture::new("s4").await;
    let graph = chain_task_graph(
        task_cfg(false, "STAY", "TERMINATE"),
        task_cfg(true, "BACK_TO_PREV", "TERMINATE"),
    );
    let def_id = create_and_publish(&f, "s4", &graph, "PFLOWB_TASK", false).await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景4").await;

    let (task_a, _, _) = flow_task_of(instance_id, "n_a", 1).await.expect("A 任务");
    drive_to_done(&f, task_a).await;
    let (task_b, _, _) = flow_task_of(instance_id, "n_b", 1).await.expect("B 任务");
    change_task(&f, task_b, TaskStatus::Doing, None)
        .await
        .expect("B 开始");
    change_task(&f, task_b, TaskStatus::Review, None)
        .await
        .expect("B 提交验收");

    let status = change_task(&f, task_b, TaskStatus::Doing, Some("上一关的条件判断错了"))
        .await
        .expect("打回");
    assert_eq!(status, "DOING", "① 打回后任务 B DOING");
    assert_eq!(
        node_status(instance_id, "n_b", 1).await.as_deref(),
        Some("REJECTED"),
        "① 本节点 REJECTED"
    );
    assert_eq!(
        node_status(instance_id, "n_a", 2).await.as_deref(),
        Some("ACTIVE"),
        "① 上一节点按 round+1 重新激活"
    );
    let (task_a2, status_a2, _) = flow_task_of(instance_id, "n_a", 2)
        .await
        .expect("① 第 2 轮的上一节点必须生成第 2 轮任务（不是重复任务）");
    assert_eq!(status_a2, "TODO");
    assert_ne!(task_a2, task_a, "① 第 2 轮是新任务行");
    assert_eq!(
        instance_row(instance_id).await.0,
        "PENDING",
        "① 实例仍 PENDING"
    );
    assert_eq!(instance_row(instance_id).await.1, 2, "① round_total = 2");
    println!("[PASS] 场景4：on_reject=BACK_TO_PREV → 上一节点 round+1 重新激活");
    f.cleanup().await;
    restore_task_accept_flow(&suspended).await;
}

// ==================== ③ 场景 5：任务取消 on_cancel=TERMINATE ====================

#[tokio::test]
async fn cancel_task_terminates_instance() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s5").await;
    let graph = chain_task_graph(
        task_cfg(false, "STAY", "TERMINATE"),
        task_cfg(false, "STAY", "TERMINATE"),
    );
    let def_id = create_and_publish(&f, "s5", &graph, "PFLOWB_TASK", false).await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景5").await;
    let (task_a, _, _) = flow_task_of(instance_id, "n_a", 1).await.expect("A 任务");
    change_task(&f, task_a, TaskStatus::Doing, None)
        .await
        .expect("开始");

    let status = change_task(
        &f,
        task_a,
        TaskStatus::Cancelled,
        Some("这张活不干了，整条线作废"),
    )
    .await
    .expect("取消任务");
    assert_eq!(status, "CANCELLED", "① 任务 CANCELLED");
    assert_eq!(
        instance_row(instance_id).await.0,
        "CANCELLED",
        "① on_cancel=TERMINATE → 实例 CANCELLED"
    );
    assert_eq!(
        node_status(instance_id, "n_a", 1).await.as_deref(),
        Some("CANCELLED"),
        "① 节点随实例终止"
    );
    let (_, _, reason) = flow_task_of(instance_id, "n_a", 1)
        .await
        .expect("任务行还在");
    let reason = reason.unwrap_or_default();
    assert!(
        reason.contains("不干了"),
        "① cancel_reason 记的是用户填的原因：{reason}"
    );
    println!("[PASS] 场景5：任务取消 → 实例终止（cancel_reason 保留原因）");
    f.cleanup().await;
}

// ==================== ③ 场景 6：实例撤回 → 未完成任务 ====================

#[tokio::test]
async fn withdraw_instance_cancels_unfinished_tasks() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s6").await;
    let def_id = create_and_publish(
        &f,
        "s6",
        &single_task_graph(task_cfg(false, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景6").await;
    let (task_id, status, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");
    assert_eq!(status, "TODO");
    let _ = task_id;

    // 零人已审 → 发起人可以撤回
    flow_handler::withdraw_instance(f.ext(), Path(instance_id.to_string()), None)
        .await
        .expect("撤回");

    assert_eq!(
        instance_row(instance_id).await.0,
        "WITHDRAWN",
        "① 实例 WITHDRAWN"
    );
    let (_, status, reason) = flow_task_of(instance_id, "n_task", 1)
        .await
        .expect("任务行还在");
    assert_eq!(
        status, "CANCELLED",
        "① 未完成任务必须 CANCELLED（不是 DONE）"
    );
    let reason = reason.unwrap_or_default();
    assert!(
        reason.contains("撤回"),
        "① cancel_reason 要说明是撤回：{reason}"
    );
    println!("[PASS] 场景6：实例撤回 → 未完成任务 CANCELLED（cancel_reason 可读）");
    f.cleanup().await;
}

// ==================== ④ on_complete=APPROVE 全链路 ====================

#[tokio::test]
async fn approve_mode_full_chain() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s7").await;
    // 先造「审批流本体」（biz_type = PFLOWB_APPROVE，默认 + 已发布）
    create_and_publish(&f, "s7appr", &approval_graph(), "PFLOWB_APPROVE", true).await;
    // 再造带 APPROVE 任务节点的流程
    let graph = single_task_graph(approve_task_cfg("STAY", "PFLOWB_APPROVE"));
    let def_id = create_and_publish(&f, "s7", &graph, "PFLOWB_TASK", false).await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景7").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");

    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    change_task(&f, task_id, TaskStatus::Done, None)
        .await
        .expect("完成（APPROVE 模式）");

    // ① 任务 DONE + 审批单已发起 + 节点仍 ACTIVE + 实例仍 PENDING
    assert_eq!(task_status(task_id).await, "DONE");
    let approval_id = task_approval_instance(task_id)
        .await
        .expect("① on_complete=APPROVE 必须回写 approval_instance_id");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("ACTIVE"),
        "① 审批没通过前节点保持 ACTIVE（不驱动下游）"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "PENDING",
        "① 实例仍 PENDING"
    );
    let approval = flow_model::find_instance(approval_id)
        .await
        .expect("查审批单")
        .expect("审批单必须存在");
    assert_eq!(
        approval.biz_type, "PFLOWB_APPROVE",
        "① 用的是 approve_biz_type"
    );
    assert_eq!(approval.biz_id, Some(task_id), "① biz_id = 任务 id");
    assert_eq!(approval.status, "PENDING");

    // ② 审批通过 → 节点 APPROVED → 实例 APPROVED
    let flow_task_id = pending_flow_task(approval_id).await;
    flow_handler::approve_task(
        f.ext(),
        Path(flow_task_id.to_string()),
        Some(Json(flow_dto::ApproveReq {
            comment: Some("同意".to_string()),
        })),
    )
    .await
    .expect("审批同意");

    assert_eq!(
        flow_model::find_instance(approval_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "APPROVED",
        "② 审批单 APPROVED"
    );
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("APPROVED"),
        "② 审批通过 → 节点 APPROVED"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "APPROVED",
        "② 节点通过后下游驱动到 END → 实例 APPROVED"
    );
    assert_eq!(
        task_status(task_id).await,
        "DONE",
        "② 任务仍是 DONE（不被回调改）"
    );
    println!("[PASS] 场景7：APPROVE 全链路（任务 DONE → 发起审批 → 通过 → 节点 APPROVED → 下游）");
    f.cleanup().await;
}

// ==================== ④ 驳回 → 按 on_reject ====================

#[tokio::test]
async fn approve_mode_reject_follows_on_reject() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s8").await;
    create_and_publish(&f, "s8appr", &approval_graph(), "PFLOWB_APPROVE", true).await;
    let graph = chain_task_graph(
        task_cfg(false, "STAY", "TERMINATE"),
        approve_task_cfg("BACK_TO_PREV", "PFLOWB_APPROVE"),
    );
    let def_id = create_and_publish(&f, "s8", &graph, "PFLOWB_TASK", false).await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景8").await;

    let (task_a, _, _) = flow_task_of(instance_id, "n_a", 1).await.expect("A 任务");
    drive_to_done(&f, task_a).await;
    let (task_b, _, _) = flow_task_of(instance_id, "n_b", 1).await.expect("B 任务");
    change_task(&f, task_b, TaskStatus::Doing, None)
        .await
        .expect("B 开始");
    change_task(&f, task_b, TaskStatus::Done, None)
        .await
        .expect("B 完成");
    let approval_id = task_approval_instance(task_b).await.expect("必须发起审批");

    let flow_task_id = pending_flow_task(approval_id).await;
    flow_handler::reject_task(
        f.ext(),
        Path(flow_task_id.to_string()),
        Json(flow_dto::RejectReq {
            comment: "金额对不上，退回上一关重核".to_string(),
            return_to: Some("NONE".to_string()),
        }),
    )
    .await
    .expect("审批驳回并终止");

    assert_eq!(
        flow_model::find_instance(approval_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "REJECTED",
        "① 审批单 REJECTED"
    );
    assert_eq!(
        node_status(instance_id, "n_b", 1).await.as_deref(),
        Some("REJECTED"),
        "① on_reject=BACK_TO_PREV → 本节点 REJECTED"
    );
    assert_eq!(
        node_status(instance_id, "n_a", 2).await.as_deref(),
        Some("ACTIVE"),
        "① 上一节点按 round+1 重新激活"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "PENDING",
        "① 流程实例仍 PENDING（没被驳回终止）"
    );
    assert_eq!(
        task_status(task_b).await,
        "DONE",
        "① 任务侧不被审批回调改写（DONE 是业务事实）"
    );
    println!("[PASS] 场景8：APPROVE 驳回 → 节点按 on_reject=BACK_TO_PREV 退回上一轮");
    f.cleanup().await;
}

// ==================== ⑤ 冲突容忍：实例已终止后再点完成 ====================

#[tokio::test]
async fn conflict_when_instance_already_cancelled() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s9").await;
    let def_id = create_and_publish(
        &f,
        "s9",
        &single_task_graph(task_cfg(false, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景9").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");

    // 让实例先终止（走真实入口：on_cancel=TERMINATE）
    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    change_task(&f, task_id, TaskStatus::Cancelled, Some("整条线作废"))
        .await
        .expect("取消任务");
    assert_eq!(instance_row(instance_id).await.0, "CANCELLED");

    // 复活任务（CANCELLED→TODO 是白名单边），再试着完成它
    change_task(&f, task_id, TaskStatus::Todo, None)
        .await
        .expect("复活");
    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始（实例已终止但任务侧照常，属 Ignored 分支）");
    let error = change_task(&f, task_id, TaskStatus::Done, None)
        .await
        .expect_err("实例已终止后完成必须 409 + 65038");
    assert_eq!(error.status, 409, "① HTTP 409");
    assert_eq!(error.code, error_code::FLOW_TASK_STATE_CONFLICT, "① 65038");
    assert_eq!(
        task_status(task_id).await,
        "DOING",
        "① 事务干净：任务必须还是 DOING（没有半截 DONE）"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "CANCELLED",
        "① 实例状态不变"
    );
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("CANCELLED"),
        "① 节点状态不变"
    );
    println!("[PASS] 场景9：冲突容忍 → 409 + 65038，两侧状态一个字节都没变");
    f.cleanup().await;
}

// ==================== ⑥ 并发：同一任务并发点两次完成 ====================

#[tokio::test]
async fn concurrent_complete_only_one_advances() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s10").await;
    let def_id = create_and_publish(
        &f,
        "s10",
        &single_task_graph(task_cfg(false, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景10").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");
    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");

    let req = || task_dto::TaskStatusChangeReq {
        to: TaskStatus::Done.code().to_string(),
        reason: None,
    };
    let (first, second) = tokio::join!(
        task_handler::change_status(f.ext(), Path(task_id.to_string()), Json(req())),
        task_handler::change_status(f.ext(), Path(task_id.to_string()), Json(req())),
    );
    let mut ok = 0;
    let mut conflict = 0;
    for result in [first, second] {
        match result {
            Ok(_) => ok += 1,
            Err(error) => {
                assert_eq!(
                    error.status, 409,
                    "① 冲突必须 409：{} {}",
                    error.code, error.msg
                );
                assert_eq!(
                    error.code,
                    error_code::FLOW_TASK_ALREADY_DONE,
                    "① 必须是 65020：{}",
                    error.msg
                );
                conflict += 1;
            }
        }
    }
    assert_eq!(ok, 1, "① 只有一次成功（推进流程）");
    assert_eq!(conflict, 1, "① 另一次 409 + 65020");
    assert_eq!(task_status(task_id).await, "DONE");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("APPROVED"),
        "① 节点只通过一次"
    );
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND node_key = 'n_task' AND round = 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("数节点行");
    assert_eq!(
        rows.first().unwrap().get_typed::<i64>("n").unwrap(),
        1,
        "① 节点行没有重复"
    );
    println!("[PASS] 场景10：并发两次完成 → 一成功一 65020，节点只通过一次");
    f.cleanup().await;
}

// ==================== ② FlowTaskOutcome 四个分支（真库各命中一次） ====================

#[tokio::test]
async fn flow_task_outcome_four_branches() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s11").await;
    let def_id = create_and_publish(
        &f,
        "s11",
        &single_task_graph(task_cfg(false, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景11").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");

    // ① NotFlowTask：三列缺失（普通任务的形态）
    let not_flow = db()
        .transaction(|tx| async move {
            task_bridge::on_task_status_changed(
                &tx,
                TaskStatusChanged {
                    task_id,
                    instance_id: None,
                    node_key: None,
                    round: None,
                    from: TaskStatus::Doing,
                    to: TaskStatus::Done,
                    operator: f.operator,
                    operator_name: "PFLOWB",
                    reason: None,
                },
            )
            .await
        })
        .await
        .expect("NotFlowTask 不该报错");
    assert!(
        matches!(not_flow, FlowTaskOutcome::NotFlowTask),
        "① 缺三列 → NotFlowTask"
    );

    // ② Ignored：实例还在跑，但这次变更不该动流程（TODO→DOING）
    let ignored = db()
        .transaction(|tx| async move {
            task_bridge::on_task_status_changed(
                &tx,
                TaskStatusChanged {
                    task_id,
                    instance_id: Some(instance_id),
                    node_key: Some("n_task"),
                    round: Some(1),
                    from: TaskStatus::Todo,
                    to: TaskStatus::Doing,
                    operator: f.operator,
                    operator_name: "PFLOWB",
                    reason: None,
                },
            )
            .await
        })
        .await
        .expect("Ignored 不该报错");
    match ignored {
        FlowTaskOutcome::Ignored { instance_status } => {
            assert_eq!(instance_status, "PENDING", "① 实例当时是 PENDING");
        }
        other => panic!("② TODO→DOING 必须是 Ignored，实得 {other:?}"),
    }

    // ③ Advanced：真完成（任务侧已经 DOING，节点 ACTIVE）
    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    let advanced = db()
        .transaction(|tx| async move {
            task_bridge::on_task_status_changed(
                &tx,
                TaskStatusChanged {
                    task_id,
                    instance_id: Some(instance_id),
                    node_key: Some("n_task"),
                    round: Some(1),
                    from: TaskStatus::Doing,
                    to: TaskStatus::Done,
                    operator: f.operator,
                    operator_name: "PFLOWB",
                    reason: None,
                },
            )
            .await
        })
        .await
        .expect("Advanced 不该报错");
    assert!(
        matches!(advanced, FlowTaskOutcome::Advanced { .. }),
        "③ 完成 → Advanced"
    );

    // ④ Conflict：节点已经 APPROVED（重复完成）
    let conflict = db()
        .transaction(|tx| async move {
            task_bridge::on_task_status_changed(
                &tx,
                TaskStatusChanged {
                    task_id,
                    instance_id: Some(instance_id),
                    node_key: Some("n_task"),
                    round: Some(1),
                    from: TaskStatus::Doing,
                    to: TaskStatus::Done,
                    operator: f.operator,
                    operator_name: "PFLOWB",
                    reason: None,
                },
            )
            .await
        })
        .await
        .expect("Conflict 以返回值给出，不是 Err");
    match conflict {
        FlowTaskOutcome::Conflict(error) => {
            assert_eq!(error.status, 409);
            assert_eq!(
                error.code,
                error_code::FLOW_TASK_ALREADY_DONE,
                "④ 重复完成 → 65020"
            );
        }
        other => panic!("④ 重复完成必须是 Conflict，实得 {other:?}"),
    }
    println!(
        "[PASS] 场景11：FlowTaskOutcome 四分支（NotFlowTask / Ignored / Advanced / Conflict）真库各命中一次"
    );
    f.cleanup().await;
}

// ==================== 补强：验收走 TASK_ACCEPT 审批时的节点推进 ====================

/// `need_review=true` + 库里有 `TASK_ACCEPT` 流程 → 提交验收自动发起审批单；
/// 审批通过后由 `on_instance_settled` 先 `REVIEW→DONE`，再推进节点。
///
/// 这条覆盖的是**两条回调链的接力**：`settle()`（任务侧，既有）+ `settle_flow_node()`
/// （节点侧，本卡）。两者顺序错了（先推节点后落任务）节点就会因为「任务还在 REVIEW」而推不动。
#[tokio::test]
async fn task_accept_approval_advances_node() {
    if ready().await.is_none() {
        return;
    }
    // 本用例**必须**有 TASK_ACCEPT 流程（上一批用例会临时停用它，这里确保是开着的）
    let suspended = suspend_task_accept_flow().await;
    restore_task_accept_flow(&suspended).await;

    let f = Fixture::new("s14").await;
    let def_id = create_and_publish(
        &f,
        "s14",
        &single_task_graph(task_cfg(true, "STAY", "TERMINATE")),
        "PFLOWB_TASK",
        false,
    )
    .await;
    let instance_id = start_instance(&f, def_id, "PFLOWB 场景14").await;
    let (task_id, _, _) = flow_task_of(instance_id, "n_task", 1).await.expect("任务");

    change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("开始");
    change_task(&f, task_id, TaskStatus::Review, None)
        .await
        .expect("提交验收");

    // ① 提交验收 → 自动发起 TASK_ACCEPT 审批单；节点仍 ACTIVE
    let approval_id = task_approval_instance(task_id)
        .await
        .expect("① 提交验收必须自动发起验收审批单");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("ACTIVE"),
        "① 审批没结论前节点保持 ACTIVE"
    );
    let approval = flow_model::find_instance(approval_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.biz_type, task_bridge::TASK_ACCEPT_BIZ_TYPE);
    assert_eq!(approval.status, "PENDING");

    // ② 审批通过 → 任务 REVIEW→DONE（settle）+ 节点 APPROVED（本卡）
    let flow_task_id = pending_flow_task(approval_id).await;
    flow_handler::approve_task(
        f.ext(),
        Path(flow_task_id.to_string()),
        Some(Json(flow_dto::ApproveReq {
            comment: Some("验收通过".to_string()),
        })),
    )
    .await
    .expect("审批同意");

    assert_eq!(task_status(task_id).await, "DONE", "② 任务 REVIEW→DONE");
    assert_eq!(
        node_status(instance_id, "n_task", 1).await.as_deref(),
        Some("APPROVED"),
        "② 节点必须 ACTIVE→APPROVED（settle 之后接力推进）"
    );
    assert_eq!(
        instance_row(instance_id).await.0,
        "APPROVED",
        "② 最后一个节点通过 → 实例 APPROVED"
    );
    println!("[PASS] 场景14：验收审批通过 → 任务 DONE + 节点 APPROVED（两条回调链接力）");
    f.cleanup().await;
}

// ==================== ⑦ 不回归：普通任务 ====================

#[tokio::test]
async fn plain_task_is_untouched() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s12").await;
    let req = task_dto::TaskCreateReq {
        title: "PFLOWB 普通任务（非流程）".to_string(),
        description: None,
        priority: None,
        need_review: Some(false),
        owner_id: None,
        parent_id: None,
        project_id: None,
        // 自由任务：不关联任何单据（P2-9 的 doc_type/doc_id 必须成对为空）
        doc_type: None,
        doc_id: None,
        start_date: None,
        due_date: None,
        estimate_hours: None,
    };
    let created = task_handler::create(f.ext(), Json(req))
        .await
        .expect("建普通任务");
    let task_id = Uuid::parse_str(&created.data.expect("CreateRes").id).expect("task id");

    // `TODO → DOING → DONE`：普通任务的老路径必须一路 200，且不产生任何流程行
    let status = change_task(&f, task_id, TaskStatus::Doing, None)
        .await
        .expect("普通任务开始");
    assert_eq!(status, "DOING");
    let status = change_task(&f, task_id, TaskStatus::Done, None)
        .await
        .expect("普通任务完成");
    assert_eq!(status, "DONE");

    let rows = db()
        .find_with_params(
            "SELECT source, flow_instance_id, flow_node_key, flow_round \
               FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await
        .expect("查普通任务");
    let row = rows.first().expect("任务必须存在");
    let source: String = row.get_typed("source").unwrap();
    let flow_instance_id: Option<Uuid> = row.get_typed("flow_instance_id").unwrap();
    assert_ne!(
        source,
        task_bridge::FLOW_SOURCE,
        "① 普通任务 source != FLOW"
    );
    assert!(flow_instance_id.is_none(), "① 普通任务没有流程实例");
    println!("[PASS] 场景12：普通任务（source != FLOW）的 change_status 行为不变");
    f.cleanup().await;
}

// ==================== 发布期：approve_biz_type 存在性 ====================

#[tokio::test]
async fn publish_rejects_missing_approve_biz_type() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("s13").await;
    // 直接建流程 + 存图，不发布（publish 要报错）
    let graph = single_task_graph(approve_task_cfg("STAY", "PFLOWB_NOT_EXIST"));
    let flow_code = format!(
        "pflowb_s13_{}",
        &Uuid::now_v7().simple().to_string()[24..32]
    );
    let create = flow_dto::DefCreateReq {
        flow_code,
        flow_name: "PFLOWB s13".to_string(),
        biz_type: "PFLOWB_TASK".to_string(),
        category: None,
        description: None,
        is_default: Some(false),
        allow_auto_approve: Some(false),
    };
    let res = flow_handler::create(f.ext(), Json(create))
        .await
        .expect("建流程");
    let data = res.data.expect("DefSaveRes");
    let def_id = Uuid::parse_str(&data.id).unwrap();
    let version_id = Uuid::parse_str(&data.version_id).unwrap();
    // 整图保存会先做结构校验（`approve_biz_type` 的存在性检查在发布 / 只校验接口上）
    flow_handler::save_graph(
        f.ext(),
        Path(version_id.to_string()),
        Json(graph_to_save_req(&graph)),
    )
    .await
    .expect("保存整图");

    let error = flow_handler::publish(f.ext(), Path(def_id.to_string()))
        .await
        .expect_err("approve_biz_type 不存在时发布必须失败");
    assert_eq!(error.status, 400);
    assert_eq!(
        error.code,
        error_code::FLOW_TASK_NODE_INVALID,
        "① 必须是 65035：{}",
        error.msg
    );
    assert!(
        error.msg.contains("PFLOWB_NOT_EXIST"),
        "① 文案要点名那个不存在的 biz_type：{}",
        error.msg
    );
    println!("[PASS] 场景13：on_complete=APPROVE 的 approve_biz_type 不存在 → 发布期 65035");
    f.cleanup().await;
}

/// 让 `graph` / `Issue` 的导入不被未使用告警（本套只用 DTO 走真实路径）。
#[allow(dead_code)]
fn _type_anchors(_: &graph::Snapshot, _: &Issue) {}
