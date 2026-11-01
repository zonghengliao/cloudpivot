//! P-FLOW-1A 端到端验收（**真库**：`cloudpivot_pflow1a` 或 `cloudpivot_test`）。
//!
//! 覆盖卡面验收 4–8：
//!
//! | 用例 | 验收项 | 断言 |
//! |---|---|---|
//! | `publish_regression_keeps_graph_hash` | ④ | 存量 4 个流程重新发布一次，`graph_hash` / 节点数 / 边数与发布前**逐字节相同** |
//! | `task_node_end_to_end_creates_one_task` | ⑤ | `START → TASK_NODE → END`：`biz_task` 新增 1 行、`source='FLOW'`、三列正确、节点 `ACTIVE`、实例仍 `PENDING` |
//! | `task_node_generation_is_idempotent` | ⑥ | 重复生成仍 1 行（`spawn` 幂等 + `uk_biz_task_flow_node` 23505） |
//! | `too_long_title_rolls_back_whole_instance` | ⑦ | 渲染成 201 字符 → 发起失败 `65039`，`biz_task` 0 行、`biz_flow_instance` 0 行 |
//! | `preview_covers_five_cases` | ⑧ | 正常 / 缺变量 strict=false / 缺变量 strict=true / 非法路径 / APPROVE 无 biz_type |
//!
//! # 连库约定（与 `tests/linkage` / `tests/testd` 同口径）
//!
//! 只连 `PFLOW_DB_URL` 指向的库，且**库名必须含 `pflow1a` 或 `cloudpivot_test`**
//! （指到别的库直接 panic —— 本套会造真实流程实例与任务）。
//!
//! 跑法：
//!
//! ```bash
//! PFLOW_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_pflow1a' \
//!   cargo test --test pflow1a -- --test-threads=1 --nocapture
//! ```
//!
//! # 为什么 `engine::step` 的幂等是「直接调 spawn + 撞唯一索引」
//!
//! `engine::Run::step` 是 `engine.rs` 的私有方法（外部不可见，只有 `start` / `approve`
//! 这些入口能触发它）。而 `step` 对 `biz_task` 的**唯一写操作**就是 `task_node::spawn`，
//! 所以本用例对「重复 step」的证明拆成两段，两段合起来等价于「step 跑两遍」：
//!
//! 1. 再调一次 `spawn` → 走前置查询返回 `AlreadyExists`；
//! 2. 绕过应用层直接 INSERT 同一 `(instance, node_key, round)` → 被
//!    `uk_biz_task_flow_node` 挡下（23505），即「就算 step 真的并发跑两遍也只有 1 行」。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::flow::graph::{
    self, AssigneeKind, AssigneeRef, EdgeSpec, FlowGraph, NodeSpec, NodeType,
};
use cloudpivot::business::flow::task_node::{self, SpawnOutcome};
use cloudpivot::business::flow::{dto as flow_dto, handler as flow_handler, model as flow_model};
use cloudpivot::business::task::{dto as task_dto, handler as task_handler};
use cloudpivot::shared::error_code;

// ==================== 连库 ====================

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `PFLOW_DB_URL`。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("PFLOW_DB_URL") else {
                println!("SKIP: 未设置 PFLOW_DB_URL（P-FLOW-1A 端到端验收需要真库）");
                return false;
            };
            assert!(
                url.contains("pflow1a") || url.contains("/cloudpivot_test"),
                "PFLOW_DB_URL 必须指向 cloudpivot_pflow1a 或 cloudpivot_test（当前：{url}）——\
                 本套测试会造真实流程实例与任务"
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
                        println!("[pflow1a] 已连接 {url}");
                        true
                    }
                    Err(error) => panic!("set_db 失败：{error}"),
                },
                Err(error) => panic!("连接 {url} 失败：{error}"),
            }
        })
        .await
}

/// 用例前置：连不上库直接 `return`（无库环境下全绿 SKIP）。
async fn ready() -> Option<()> {
    if ensure_db().await { Some(()) } else { None }
}

/// 构造一份带权限的 claims（`#[nik::permission]` 由中间件执行，直接调 handler 时不校验；
// 但 handler 内部的 `claims.has_permission(...)` 会用到）。
fn claims(user: Uuid, name: &str, permissions: &[&str]) -> Extension<AuthClaims> {
    Extension(AuthClaims(TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(name.to_string()),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: permissions.iter().map(|p| p.to_string()).collect(),
        token_type: Some(TokenType::Access),
        exp: 4_000_000_000,
        iat: 1_700_000_000,
        extra: std::collections::HashMap::new(),
    }))
}

// ==================== 夹具 ====================

/// 一次用例的夹具：一个角色 + 两个人（发起人 / 协作同事）。
struct Fixture {
    tag: String,
    role: Uuid,
    operator: Uuid,
    colleague: Uuid,
}

impl Fixture {
    async fn new(tag: &str, suffix: &str) -> Self {
        let role = Uuid::now_v7();
        let operator = Uuid::now_v7();
        let colleague = Uuid::now_v7();
        let code = format!("PFLOW-{tag}-{suffix}");
        db().execute_with_params(
            "INSERT INTO sys_role (id, role_code, role_name, data_scope, status) \
             VALUES (?::uuid, ?, ?, 4, 1)",
            vec![
                Value::from(&role),
                Value::Text(code.clone()),
                Value::Text(format!("PFLOW {tag} 夹具角色")),
            ],
        )
        .await
        .expect("建夹具角色");
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
             VALUES (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN'), \
                    (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN')",
            vec![
                Value::from(&operator),
                Value::Text(format!("{code}-op")),
                Value::Text(format!("{tag} 发起人")),
                Value::from(&colleague),
                Value::Text(format!("{code}-cb")),
                Value::Text(format!("{tag} 同事")),
            ],
        )
        .await
        .expect("建夹具用户");
        db().execute_with_params(
            "INSERT INTO sys_user_role (id, user_id, role_id) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid), (gen_random_uuid(), ?::uuid, ?::uuid)",
            vec![
                Value::from(&operator),
                Value::from(&role),
                Value::from(&colleague),
                Value::from(&role),
            ],
        )
        .await
        .expect("建夹具用户角色");
        Self {
            tag: tag.to_string(),
            role,
            operator,
            colleague,
        }
    }

    /// 清掉本用例造的一切（按精确 id / 流程编码前缀）。
    async fn cleanup(&self) {
        let prefix = format!("pflow_{}_", self.tag.to_ascii_lowercase());
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_task WHERE flow_instance_id IN ( \
                     SELECT id FROM biz_flow_instance WHERE flow_code LIKE ? )",
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_instance WHERE flow_code LIKE ?",
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await;
        // 流程定义：节点 / 边 / 版本靠 CASCADE，def 先按编码前缀删
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_node WHERE version_id IN ( \
                     SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                      WHERE d.flow_code LIKE ? )",
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_edge WHERE version_id IN ( \
                     SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                      WHERE d.flow_code LIKE ? )",
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_def WHERE flow_code LIKE ?",
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user_role WHERE role_id = ?::uuid",
                vec![Value::from(&self.role)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(&self.operator), Value::from(&self.colleague)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_role WHERE id = ?::uuid",
                vec![Value::from(&self.role)],
            )
            .await;
    }
}

// ==================== 建流程 / 发布 / 发起 ====================

/// 一份 `START → TASK_NODE → END` 的图（任务节点指派来源可切换）。
fn task_flow_graph(
    assignee_type: AssigneeKind,
    role_id: Option<Uuid>,
    task_config: serde_json::Value,
) -> FlowGraph {
    let mut task = NodeSpec::new("n_task", "催货", NodeType::Task);
    task.assignee_type = Some(assignee_type);
    if let Some(role_id) = role_id {
        task.assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: role_id.to_string(),
            sort_order: 1,
        }];
    }
    task.empty_assignee = "ERROR".to_string();
    task.timeout_action = "REMIND".to_string();
    task.config = serde_json::json!({ "task": task_config });
    FlowGraph {
        nodes: vec![
            NodeSpec::new("n_start", "开始", NodeType::Start),
            task,
            NodeSpec::new("n_end", "结束", NodeType::End),
        ],
        edges: vec![
            EdgeSpec::new("e1", "n_start", "n_task"),
            EdgeSpec::new("e2", "n_task", "n_end"),
        ],
    }
}

/// 建流程 + 存图 + 发布，返回 `(def_id, version_id)`。
async fn create_and_publish(f: &Fixture, tag: &str, graph: FlowGraph) -> (Uuid, Uuid, String) {
    let flow_code = format!(
        "pflow_{}_{}",
        tag.to_ascii_lowercase(),
        &Uuid::now_v7().simple().to_string()[24..32]
    );
    let create = flow_dto::DefCreateReq {
        flow_code: flow_code.clone(),
        flow_name: format!("PFLOW {tag} 任务流程"),
        biz_type: "PFLOW_TEST".to_string(),
        category: None,
        description: None,
        is_default: Some(false),
        allow_auto_approve: Some(false),
    };
    let res = flow_handler::create(claims(f.operator, "op", &[]), Json(create))
        .await
        .unwrap_or_else(|error| panic!("建流程失败：{} {}", error.code, error.msg));
    let data = res.data.expect("建流程必须返回 DefSaveRes");
    let def_id = Uuid::parse_str(&data.id).expect("def id");
    let version_id = Uuid::parse_str(&data.version_id).expect("version id");

    // 用 DTO 走一遍真实保存路径（含 into_graph 的结构校验）
    let save = graph_to_save_req(&graph);
    flow_handler::save_graph(
        claims(f.operator, "op", &[]),
        Path(version_id.to_string()),
        Json(save),
    )
    .await
    .unwrap_or_else(|error| panic!("保存整图失败：{} {}", error.code, error.msg));

    let published = flow_handler::publish(claims(f.operator, "op", &[]), Path(def_id.to_string()))
        .await
        .unwrap_or_else(|error| panic!("发布失败：{} {}", error.code, error.msg));
    let hash = published.data.expect("发布必须返回 PublishRes").graph_hash;
    (def_id, version_id, hash)
}

/// `FlowGraph` → `GraphSaveReq`（测试里只有这一条反向转换，够用即可）。
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
                self_approval: None,
                allow_robot: None,
                join_mode: None,
                fork_mode: None,
                timeout_hours: None,
                timeout_action: Some(node.timeout_action.clone()),
                remind_before_hours: None,
                max_remind_times: None,
                cc_on_approved: None,
                config: Some(node.config.clone()),
                assignees: node
                    .assignees
                    .iter()
                    .map(|assignee| flow_dto::AssigneeReq {
                        kind: assignee.kind.clone(),
                        ref_id: assignee.ref_id.clone(),
                        sort_order: Some(assignee.sort_order),
                    })
                    .collect(),
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
        change_note: Some("P-FLOW-1A 端到端".to_string()),
    }
}

/// 发起实例（走真实 handler）。
async fn start_instance(
    f: &Fixture,
    def_id: Uuid,
    title: &str,
    form: serde_json::Value,
) -> std::result::Result<Uuid, ApiError> {
    let req = flow_dto::InstanceCreateReq {
        def_id: Some(def_id.to_string()),
        flow_code: None,
        biz_type: None,
        biz_id: None,
        biz_no: Some(format!(
            "PFLOW-{}",
            &Uuid::now_v7().simple().to_string()[24..32]
        )),
        title: title.to_string(),
        form_data: Some(form),
    };
    let res = flow_handler::create_instance(claims(f.operator, "op", &[]), Json(req)).await?;
    let data = res.data.expect("发起必须返回 ActionRes");
    Ok(Uuid::parse_str(&data.instance_id).expect("instance id"))
}

/// 数一数某个实例下的流程任务行。
async fn count_flow_tasks(instance_id: Uuid) -> i64 {
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_task WHERE flow_instance_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("count biz_task");
    rows.first()
        .map(|row| row.get_typed("n").unwrap())
        .unwrap_or(0)
}

// ==================== ④ 发布回归 ====================

/// 存量 4 个流程「另存草稿 → 重新发布」后，`graph_hash` 与发布前**相同**。
///
/// 这是「R17–R21 对老节点零影响」的硬证据：hash 是 canonical 快照的摘要，
/// 只要快照序列化多一个字段 / 少一个字段，hash 立刻变化。
#[tokio::test]
async fn publish_regression_keeps_graph_hash() {
    if ready().await.is_none() {
        return;
    }
    // 存量已发布流程（§6.4 SQL ③ 的口径：`deleted_at IS NULL` 且状态 PUBLISHED）
    let defs = db()
        .find_with_params(
            "SELECT id, flow_code FROM biz_flow_def \
              WHERE deleted_at IS NULL AND status = 'PUBLISHED' ORDER BY flow_code",
            Vec::new(),
        )
        .await
        .expect("列已发布流程")
        .iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").expect("def id"),
                row.get_typed::<String>("flow_code").expect("flow_code"),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !defs.is_empty(),
        "库里必须已有发布的流程（否则这条回归没有意义）"
    );
    let operator = Uuid::now_v7();
    for (def_id, flow_code) in &defs {
        let def_id = *def_id;
        let flow_code = flow_code.clone();
        let def_row = flow_model::find_def(def_id)
            .await
            .expect("查流程定义")
            .unwrap_or_else(|| panic!("流程 {flow_code} 不存在"));
        let before_version_id = def_row
            .current_version_id
            .unwrap_or_else(|| panic!("流程 {flow_code} 没有当前版本"));
        let before = flow_model::find_version(before_version_id)
            .await
            .expect("查当前版本")
            .unwrap_or_else(|| panic!("流程 {flow_code} 的当前版本行缺失"));
        let before_hash = before.graph_hash.clone().unwrap_or_default();
        assert!(!before_hash.is_empty(), "{flow_code} 的 graph_hash 为空");

        // ① 另存为新草稿（复制当前版本的整图）
        let draft = flow_handler::create_draft(
            claims(operator, "pflow-regression", &[]),
            Path(def_id.to_string()),
            Json(flow_dto::DraftReq {
                source_version_id: None,
                change_note: Some("P-FLOW-1A 发布回归".to_string()),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{flow_code} 另存草稿失败：{} {}", error.code, error.msg));
        let draft_id =
            Uuid::parse_str(&draft.data.expect("草稿回执").version_id).expect("draft id");

        // ② 重新发布（跑 R1–R21 + 重新算 hash）
        let published = flow_handler::publish(
            claims(operator, "pflow-regression", &[]),
            Path(def_id.to_string()),
        )
        .await
        .unwrap_or_else(|error| panic!("{flow_code} 重新发布失败：{} {}", error.code, error.msg));
        let after_hash = published.data.expect("发布回执").graph_hash;

        // ③ 断言 hash / 节点数 / 边数逐一相同
        let after = flow_model::find_version(draft_id)
            .await
            .expect("查新版本")
            .expect("新版本必须存在");
        assert_eq!(
            after_hash, before_hash,
            "{flow_code} 重新发布后 graph_hash 变了（{before_hash} → {after_hash}）：\
             说明快照口径被改动，存量 PUBLISHED 版本全部失效"
        );
        assert_eq!(
            after.node_count, before.node_count,
            "{flow_code} 节点数变了"
        );
        assert_eq!(after.edge_count, before.edge_count, "{flow_code} 边数变了");
        println!(
            "[④] {flow_code} v{} → 重新发布 hash 不变（{before_hash}），nodes={} edges={}",
            before.version_no, before.node_count, before.edge_count
        );
    }
}

// ==================== ⑤ 端到端生成任务 ====================

/// `START → TASK_NODE → END` 发起后：`biz_task` 1 行、三列正确、节点 ACTIVE、实例 PENDING。
#[tokio::test]
async fn task_node_end_to_end_creates_one_task() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("E2E", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let graph = task_flow_graph(
        AssigneeKind::Initiator,
        None,
        serde_json::json!({
            "title_template": "{{instance.biz_no}} 催货：{{form.supplier_name}}",
            "description_template": "金额 {{form.amount}} 元",
            "on_complete": "DIRECT",
            "due": {"mode": "RELATIVE", "amount": 3, "unit": "WORKDAY"}
        }),
    );
    let (def_id, _version_id, _hash) = create_and_publish(&f, "e2e", graph).await;

    let instance_id = start_instance(
        &f,
        def_id,
        "PFLOW 端到端",
        serde_json::json!({"supplier_name": "华东金属", "amount": 125000}),
    )
    .await
    .unwrap_or_else(|error| panic!("发起失败：{} {}", error.code, error.msg));

    // ① biz_task 恰好 1 行
    let rows = db()
        .find_with_params(
            "SELECT id, task_no, title, description, status, source, priority, need_review, \
                    owner_id, creator_id, flow_instance_id, flow_node_key, flow_round, due_date \
               FROM biz_task WHERE flow_instance_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查 biz_task");
    assert_eq!(rows.len(), 1, "一个任务节点必须只生成 1 张任务");
    let row = rows.first().expect("至少 1 行");
    let source: String = row.get_typed("source").unwrap();
    let title: String = row.get_typed("title").unwrap();
    let description: Option<String> = row.get_typed("description").unwrap();
    let owner_id: Uuid = row.get_typed("owner_id").unwrap();
    let creator_id: Uuid = row.get_typed("creator_id").unwrap();
    let node_key: String = row.get_typed("flow_node_key").unwrap();
    let round: i32 = row.get_typed("flow_round").unwrap();
    let instance_col: Uuid = row.get_typed("flow_instance_id").unwrap();
    let priority: String = row.get_typed("priority").unwrap();
    let need_review: bool = row.get_typed("need_review").unwrap();
    let due_date: Option<chrono::NaiveDate> = row.get_typed("due_date").unwrap();

    assert_eq!(source, "FLOW", "source 必须是 FLOW");
    assert_eq!(instance_col, instance_id, "flow_instance_id 必须指向本实例");
    assert_eq!(node_key, "n_task", "flow_node_key 必须是节点 key");
    assert_eq!(round, 1, "第一轮生成 round=1");
    assert!(
        title.starts_with("PFLOW-") && title.ends_with(" 催货：华东金属"),
        "标题必须渲染点号变量（{{{{instance.biz_no}}}} 催货：{{{{form.supplier_name}}}}），实际：{title}"
    );
    assert_eq!(description.as_deref(), Some("金额 125000 元"));
    assert_eq!(priority, "P2", "未配置时默认 P2");
    assert!(!need_review);
    assert_eq!(owner_id, f.operator, "INITIATOR 来源 → 负责人是发起人");
    assert_eq!(creator_id, f.operator, "creator = 流程发起人");
    assert!(due_date.is_some(), "RELATIVE 3 工作日必须有截止日");

    // ② 节点 ACTIVE、实例仍 PENDING
    let nodes = db()
        .find_with_params(
            "SELECT status FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND node_key = 'n_task'",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查节点");
    assert_eq!(nodes.len(), 1);
    let node_status: String = nodes[0].get_typed("status").unwrap();
    assert_eq!(node_status, "ACTIVE", "任务节点必须阻塞在 ACTIVE");
    let instance = flow_model::find_instance(instance_id)
        .await
        .expect("查实例")
        .expect("实例必须存在");
    assert_eq!(instance.status, "PENDING", "任务没完成，实例必须仍 PENDING");
    println!(
        "[⑤] 实例 {instance_id} 生成 1 张任务，节点 ACTIVE，实例 PENDING，截止日 {due_date:?}"
    );

    f.cleanup().await;
}

// ==================== ⑤b 多人解析 → 1 owner + 协作人 ====================

/// 角色里 2 个人 → **1 张任务**（owner = user_id 最小的），另一人挂 `biz_task_member`。
#[tokio::test]
async fn role_multi_resolution_makes_one_task_with_members() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("MULTI", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let graph = task_flow_graph(
        AssigneeKind::Role,
        Some(f.role),
        serde_json::json!({"title_template": "多人任务", "on_complete": "DIRECT"}),
    );
    let (def_id, _, _) = create_and_publish(&f, "multi", graph).await;
    let instance_id = start_instance(&f, def_id, "多人解析", serde_json::json!({}))
        .await
        .unwrap_or_else(|error| panic!("发起失败：{} {}", error.code, error.msg));

    assert_eq!(
        count_flow_tasks(instance_id).await,
        1,
        "多人必须只生成 1 张任务"
    );

    let rows = db()
        .find_with_params(
            "SELECT t.owner_id, (SELECT count(*) FROM biz_task_member m WHERE m.task_id = t.id) AS members \
               FROM biz_task t WHERE t.flow_instance_id = ?::uuid AND t.deleted_at IS NULL",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查任务与协作人");
    let owner: Uuid = rows[0].get_typed("owner_id").unwrap();
    let members: i64 = rows[0].get_typed("members").unwrap();
    let expected_owner = std::cmp::min(f.operator, f.colleague);
    assert_eq!(
        owner, expected_owner,
        "owner 必须是按 user_id 排序的第一个人"
    );
    assert_eq!(members, 1, "其余解析出来的人必须挂协作人");
    println!("[⑤b] 角色 2 人 → 1 张任务，owner={owner}，协作人 {members} 个");

    f.cleanup().await;
}

// ==================== ⑥ 幂等 ====================

/// 重复生成仍 1 行：`spawn` 前置查询命中 + 唯一索引 23505 兜底。
#[tokio::test]
async fn task_node_generation_is_idempotent() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("IDEM", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let graph = task_flow_graph(
        AssigneeKind::Initiator,
        None,
        serde_json::json!({"title_template": "幂等任务", "on_complete": "DIRECT"}),
    );
    let (def_id, _version_id, _hash) = create_and_publish(&f, "idem", graph).await;
    let instance_id = start_instance(&f, def_id, "幂等", serde_json::json!({}))
        .await
        .unwrap_or_else(|error| panic!("发起失败：{} {}", error.code, error.msg));
    assert_eq!(count_flow_tasks(instance_id).await, 1);

    // ① 再走一次 spawn（= 引擎 step 对 biz_task 的唯一写路径）→ AlreadyExists
    let instance = flow_model::find_instance(instance_id)
        .await
        .expect("查实例")
        .expect("实例存在");
    let raw = flow_model::load_snapshot(instance.version_id)
        .await
        .expect("读快照")
        .expect("快照必须存在");
    let snapshot: graph::Snapshot = serde_json::from_str(&raw).expect("快照必须是合法 JSON");
    let spec = snapshot
        .nodes
        .iter()
        .find(|node| node.key == "n_task")
        .expect("快照里必须有任务节点")
        .clone();
    let cfg = cloudpivot::business::flow::node_config::parse_task_config(&spec.config["task"])
        .expect("配置合法");
    let ctx = instance.assignee_ctx();
    let eval = graph::EvalContext {
        form: serde_json::Map::new(),
        initiator: serde_json::Map::new(),
        instance: serde_json::Map::new(),
        task: serde_json::Map::new(),
    };
    let outcome = db()
        .transaction(|tx| {
            let instance = instance.clone();
            let spec = spec.clone();
            let cfg = cfg.clone();
            let ctx = ctx.clone();
            let eval = eval.clone();
            async move { task_node::spawn(&tx, &instance, &spec, 1, &cfg, &ctx, &eval).await }
        })
        .await
        .expect("第二次 spawn 不该报错");
    match outcome {
        SpawnOutcome::AlreadyExists { task_id } => {
            assert!(task_id.is_some(), "幂等命中必须能回出已有任务 id");
        }
        other => panic!("第二次 spawn 必须返回 AlreadyExists，实际 {other:?}"),
    }
    assert_eq!(
        count_flow_tasks(instance_id).await,
        1,
        "幂等命中后仍必须是 1 行"
    );

    // ② 绕过应用层再 INSERT 一次 → 被 uk_biz_task_flow_node 挡下（23505）
    let duplicate = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review, \
                                   owner_id, creator_id, source, flow_instance_id, flow_node_key, flow_round) \
             VALUES (gen_random_uuid(), 'PFLOW-DUP-1', '重复', 'TODO', 'P2', 0, false, \
                     ?::uuid, ?::uuid, 'FLOW', ?::uuid, 'n_task', 1)",
            vec![
                Value::from(&f.operator),
                Value::from(&f.operator),
                Value::from(&instance_id),
            ],
        )
        .await;
    assert!(duplicate.is_err(), "唯一索引必须挡住同一节点同一轮的第二行");
    let error = duplicate.expect_err("必须报错");
    assert!(
        task_node::is_flow_node_duplicate(&error),
        "必须是 uk_biz_task_flow_node 冲突（23505），实际：{error}"
    );
    assert_eq!(count_flow_tasks(instance_id).await, 1);
    println!("[⑥] 重复生成仍 1 行（前置查询 + 唯一索引双保险）");

    f.cleanup().await;
}

// ==================== ⑦ 失败回滚 ====================

/// `title_template` 渲染成 201 字符 → 发起失败 `65039`，且**整笔回滚**（0 任务 0 实例）。
#[tokio::test]
async fn too_long_title_rolls_back_whole_instance() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("ROLL", &Uuid::now_v7().simple().to_string()[24..32]).await;
    // 模板很短（发布期 R18 的长度校验是 1..=200 字符），但**渲染结果**会到 201 字符
    let graph = task_flow_graph(
        AssigneeKind::Initiator,
        None,
        serde_json::json!({
            "title_template": "{{form.blob}}",
            "on_complete": "DIRECT"
        }),
    );
    let (def_id, _, _) = create_and_publish(&f, "roll", graph).await;

    let blob = "长".repeat(201);
    let error = start_instance(&f, def_id, "回滚", serde_json::json!({ "blob": blob }))
        .await
        .expect_err("201 字符的标题必须发起失败");
    assert_eq!(
        error.code,
        error_code::FLOW_PREVIEW_INVALID,
        "必须是 65039：{}",
        error.msg
    );
    assert_eq!(error.status, 400);

    // 整笔回滚：实例 / 节点 / 任务一行都不能留
    let count = move |sql: &'static str| async move {
        let rows = db()
            .find_with_params(sql, vec![Value::from(&def_id)])
            .await
            .expect("计数");
        rows.first()
            .map(|row| row.get_typed::<i64>("n").unwrap())
            .unwrap_or(0)
    };
    let instances =
        count("SELECT count(*) AS n FROM biz_flow_instance WHERE def_id = ?::uuid").await;
    assert_eq!(instances, 0, "发起失败必须不留下半截实例");
    let tasks = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_task t \
               WHERE t.source::text = 'FLOW' AND t.creator_id = ?::uuid",
            vec![Value::from(&f.operator)],
        )
        .await
        .expect("数流程任务");
    assert_eq!(
        tasks
            .first()
            .map(|row| row.get_typed::<i64>("n").unwrap())
            .unwrap_or(0),
        0,
        "发起失败必须不留下任务"
    );
    println!("[⑦] 标题 201 字符 → 65039，实例 0 行、任务 0 行");

    f.cleanup().await;
}

// ==================== ⑨ 任务列表：source=FLOW 筛选 + flow 段 ====================

/// `GET /business/task/page?source=FLOW` 只回流程生成的任务，且出参带 `flow` 段。
#[tokio::test]
async fn task_page_filters_by_flow_source() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("LIST", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let graph = task_flow_graph(
        AssigneeKind::Initiator,
        None,
        serde_json::json!({"title_template": "列表筛选任务", "on_complete": "DIRECT"}),
    );
    let (def_id, _, _) = create_and_publish(&f, "list", graph).await;
    let instance_id = start_instance(&f, def_id, "列表筛选", serde_json::json!({}))
        .await
        .unwrap_or_else(|error| panic!("发起失败：{} {}", error.code, error.msg));

    // `source=FLOW`：必须命中这张流程任务，且 `flow` 段三列齐全
    let query = task_dto::TaskPageQuery {
        page: Some(1),
        page_size: Some(50),
        status: None,
        statuses: None,
        priority: None,
        owner_id: Some(f.operator.to_string()),
        project_id: None,
        source: Some("FLOW".to_string()),
        due_from: None,
        due_to: None,
        keyword: None,
        root_only: None,
    };
    let res = task_handler::page(
        claims(f.operator, "op", &["business:task:read"]),
        Query(query.clone()),
    )
    .await
    .unwrap_or_else(|error| panic!("task/page?source=FLOW 失败：{} {}", error.code, error.msg));
    let data = res.data.expect("分页回执");
    assert_eq!(data.total, 1, "该负责人名下只应有 1 张流程任务");
    let card = data.list.first().expect("必须有一条");
    assert_eq!(card.source, "FLOW");
    let flow = card.flow.as_ref().expect("流程任务必须带 flow 段");
    assert_eq!(flow.instance_id, instance_id.to_string());
    assert_eq!(flow.node_key, "n_task");
    assert_eq!(flow.round, 1);

    // `source=SELF`：同一条件下不该命中（证明筛选真的生效，不是摆设）
    let mut self_query = query.clone();
    self_query.source = Some("SELF".to_string());
    let res = task_handler::page(
        claims(f.operator, "op", &["business:task:read"]),
        Query(self_query),
    )
    .await
    .expect("task/page?source=SELF 不该报错");
    assert_eq!(
        res.data.expect("分页回执").total,
        0,
        "source=SELF 不该命中流程任务"
    );

    // 非法来源 → 400（不静默忽略）
    let mut bad = query;
    bad.source = Some("NOPE".to_string());
    let error = task_handler::page(
        claims(f.operator, "op", &["business:task:read"]),
        Query(bad),
    )
    .await
    .expect_err("非法来源必须 400");
    assert_eq!(error.status, 400);
    println!("[⑨] source=FLOW 命中 1 条并带 flow 段；source=SELF 0 条；非法值 400");

    f.cleanup().await;
}

// ==================== ⑧ 预演 5 个 case ====================

/// `/node/task/preview` 的五个 case，错误码与设计 §3.3 的表逐条一致。
#[tokio::test]
async fn preview_covers_five_cases() {
    if ready().await.is_none() {
        return;
    }
    // ① 正常：变量齐全 → 渲染出标题 + 截止日
    let ok_req = flow_dto::TaskPreviewReq {
        config: serde_json::json!({
            "title_template": "{{instance.biz_no}} 催货：{{form.supplier_name}}",
            "description_template": "请于 {{form.delivery_date}} 前催到货",
            "on_complete": "DIRECT",
            "due": {"mode": "RELATIVE", "amount": 3, "unit": "WORKDAY"}
        }),
        assignee: None,
        vars: Some(serde_json::json!({
            "form": {"supplier_name": "华东金属", "delivery_date": "2026-11-04"},
            "instance": {"biz_no": "PO202611010001"}
        })),
        biz_type: Some("PURCHASE_ORDER".to_string()),
        node_name: Some("催货".to_string()),
        due_base: Some(
            chrono::DateTime::parse_from_rfc3339("2026-11-01T09:00:00+08:00")
                .expect("固定时刻")
                .with_timezone(&chrono::Utc),
        ),
        strict: None,
    };
    let res = flow_handler::task_preview(Json(ok_req))
        .await
        .expect("正常 case 必须 200");
    let data = res.data.expect("必须有回执");
    assert!(data.ok);
    let task = data.task.expect("必须渲染出任务");
    assert_eq!(task.title, "PO202611010001 催货：华东金属");
    assert_eq!(task.description, "请于 2026-11-04 前催到货");
    assert_eq!(
        task.due_date.as_deref(),
        Some("2026-11-04"),
        "周日 + 3 工作日 = 周三"
    );

    // ② 缺变量 strict=false → 可见占位 + warning（不报错）
    let lenient = flow_dto::TaskPreviewReq {
        config: serde_json::json!({
            "title_template": "{{instance.biz_no}} 催货：{{form.supplier_name}}",
            "on_complete": "DIRECT"
        }),
        assignee: None,
        vars: Some(serde_json::json!({"instance": {"biz_no": "PO1"}})),
        biz_type: Some("PURCHASE_ORDER".to_string()),
        node_name: None,
        due_base: None,
        strict: Some(false),
    };
    let res = flow_handler::task_preview(Json(lenient))
        .await
        .expect("非严格模式缺变量不该 400");
    let data = res.data.expect("回执");
    assert_eq!(
        data.task.expect("任务").title,
        "PO1 催货：〔form.supplier_name〕",
        "缺变量必须渲染成可见占位"
    );
    assert!(
        data.warnings
            .iter()
            .any(|w| w.contains("form.supplier_name")),
        "必须给出缺变量告警：{:?}",
        data.warnings
    );

    // ③ 缺变量 strict=true → 400 + 65039
    let strict_req = flow_dto::TaskPreviewReq {
        config: serde_json::json!({
            "title_template": "{{instance.biz_no}} 催货：{{form.supplier_name}}",
            "on_complete": "DIRECT"
        }),
        assignee: None,
        vars: Some(serde_json::json!({"instance": {"biz_no": "PO1"}})),
        biz_type: Some("PURCHASE_ORDER".to_string()),
        node_name: None,
        due_base: None,
        strict: Some(true),
    };
    let error = flow_handler::task_preview(Json(strict_req))
        .await
        .expect_err("严格模式缺变量必须 400");
    assert_eq!(error.code, error_code::FLOW_PREVIEW_INVALID);
    assert_eq!(error.status, 400);

    // ④ 非法变量路径 → 400 + 65037
    let bad_path = flow_dto::TaskPreviewReq {
        config: serde_json::json!({
            "title_template": "{{order.amount}} 催货",
            "on_complete": "DIRECT"
        }),
        assignee: None,
        vars: Some(serde_json::json!({"order": {"amount": 1}})),
        biz_type: Some("PURCHASE_ORDER".to_string()),
        node_name: None,
        due_base: None,
        strict: None,
    };
    let error = flow_handler::task_preview(Json(bad_path))
        .await
        .expect_err("白名单外的变量必须 400");
    assert_eq!(error.code, error_code::FLOW_TEMPLATE_VAR_INVALID);
    assert_eq!(error.status, 400);

    // ⑤ APPROVE 无 biz_type → 400 + 65034
    let approve = flow_dto::TaskPreviewReq {
        config: serde_json::json!({
            "title_template": "审批后完成",
            "on_complete": "APPROVE"
        }),
        assignee: None,
        vars: None,
        biz_type: Some("PURCHASE_ORDER".to_string()),
        node_name: None,
        due_base: None,
        strict: None,
    };
    let error = flow_handler::task_preview(Json(approve))
        .await
        .expect_err("APPROVE 缺 biz_type 必须 400");
    assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
    assert_eq!(error.status, 400);
    println!("[⑧] 预演 5 个 case 的错误码与设计 §3.3 表逐条一致");
}

// ==================== ⑩ `GET /task/{id}/flow` 带任务走向（P-FLOW-1C 补漏） ====================

/// 取某实例生成的流程任务 id（一个任务节点只生成 1 行）。
async fn task_of_instance(instance_id: Uuid) -> Uuid {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_task WHERE flow_instance_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查生成的流程任务");
    assert_eq!(rows.len(), 1, "一个任务节点只应生成 1 张任务");
    rows[0].get_typed("id").expect("task id")
}

/// 起一张实例 → 打 `/business/task/{id}/flow` → 回三个「流程走向」字段。
async fn flow_detail_of(
    f: &Fixture,
    tag: &str,
    task_config: serde_json::Value,
) -> task_dto::TaskFlowDetailRes {
    let graph = task_flow_graph(AssigneeKind::Initiator, None, task_config);
    let (def_id, _, _) = create_and_publish(f, tag, graph).await;
    let instance_id = start_instance(
        f,
        def_id,
        &format!("PFLOW {tag} 走向"),
        serde_json::json!({}),
    )
    .await
    .unwrap_or_else(|error| panic!("发起失败：{} {}", error.code, error.msg));
    let task_id = task_of_instance(instance_id).await;
    let res = task_handler::flow_of_task(
        claims(f.operator, "op", &["business:task:read"]),
        Path(task_id.to_string()),
    )
    .await
    .unwrap_or_else(|error| panic!("GET /task/{{id}}/flow 失败：{} {}", error.code, error.msg));
    res.data
        .expect("flow 回执")
        .flow
        .expect("流程任务必须有 flow 段")
}

/// `on_complete=VERIFY`（+ `need_review=true`）：接口必须回 `VERIFY`，不是 `DIRECT`。
///
/// 这条是 P-FLOW-1C 前端降级的**回归钉子**：旧实现完全不带这三个字段，前端只能按
/// `need_review` 推断成 `VERIFY`；而 `APPROVE` 会被推断成 `DIRECT`（副文案缺失）。
#[tokio::test]
async fn flow_detail_carries_on_complete_verify() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("ONCV", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let flow = flow_detail_of(
        &f,
        "oncv",
        serde_json::json!({
            "title_template": "验收任务",
            "need_review": true,
            "on_complete": "VERIFY",
            "on_reject": "STAY",
            "on_cancel": "BACK_TO_PREV"
        }),
    )
    .await;
    assert_eq!(
        flow.on_complete.as_deref(),
        Some("VERIFY"),
        "on_complete 必须回快照里的 VERIFY"
    );
    assert_eq!(
        flow.on_reject.as_deref(),
        Some("STAY"),
        "on_reject 必须回 STAY"
    );
    assert_eq!(
        flow.on_cancel.as_deref(),
        Some("BACK_TO_PREV"),
        "on_cancel 必须回 BACK_TO_PREV"
    );
    assert_eq!(flow.node_key, "n_task");
    assert_eq!(flow.node_name, "催货", "节点名仍取快照");
    assert_eq!(flow.node_status, "ACTIVE", "任务未完成 → 节点 ACTIVE");
    println!(
        "[⑩] VERIFY：on_complete={:?} on_reject={:?} on_cancel={:?}",
        flow.on_complete, flow.on_reject, flow.on_cancel
    );

    f.cleanup().await;
}

/// 反例 `on_complete=DIRECT`：同样三个字段都要有值（不能因为「默认值」就省掉）。
#[tokio::test]
async fn flow_detail_carries_on_complete_direct() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("ONCD", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let flow = flow_detail_of(
        &f,
        "oncd",
        serde_json::json!({
            "title_template": "直接完成任务",
            "on_complete": "DIRECT",
            "on_reject": "BACK_TO_PREV",
            "on_cancel": "TERMINATE"
        }),
    )
    .await;
    assert_eq!(flow.on_complete.as_deref(), Some("DIRECT"));
    assert_eq!(flow.on_reject.as_deref(), Some("BACK_TO_PREV"));
    assert_eq!(flow.on_cancel.as_deref(), Some("TERMINATE"));
    println!(
        "[⑩] DIRECT：on_complete={:?} on_reject={:?} on_cancel={:?}",
        flow.on_complete, flow.on_reject, flow.on_cancel
    );

    f.cleanup().await;
}

/// 非流程任务（人类自建）→ `flow` 为 `None`（三个字段自然不出现，**不是空串兜底**）。
#[tokio::test]
async fn flow_detail_is_null_for_manual_task() {
    if ready().await.is_none() {
        return;
    }
    let f = Fixture::new("ONCM", &Uuid::now_v7().simple().to_string()[24..32]).await;
    let create = task_dto::TaskCreateReq {
        title: "PFLOW 自建任务（无流程）".to_string(),
        description: None,
        priority: Some("P2".to_string()),
        owner_id: Some(f.operator.to_string()),
        project_id: None,
        parent_id: None,
        start_date: None,
        due_date: None,
        estimate_hours: None,
        need_review: None,
    };
    let res = task_handler::create(
        claims(f.operator, "op", &["business:task:create"]),
        Json(create),
    )
    .await
    .unwrap_or_else(|error| panic!("建任务失败：{} {}", error.code, error.msg));
    let task_id = res.data.expect("建任务回执").id;
    let flow_res = task_handler::flow_of_task(
        claims(f.operator, "op", &["business:task:read"]),
        Path(task_id.clone()),
    )
    .await
    .unwrap_or_else(|error| panic!("GET /task/{{id}}/flow 失败：{} {}", error.code, error.msg));
    assert!(
        flow_res.data.expect("flow 回执").flow.is_none(),
        "手工任务的 flow 必须是 null（前端据此隐藏卡片）"
    );
    let _ = db()
        .execute_with_params(
            "DELETE FROM biz_task WHERE id = ?::uuid",
            vec![Value::Text(task_id)],
        )
        .await;
    println!("[⑩] 非流程任务 → flow = null");
    f.cleanup().await;
}
