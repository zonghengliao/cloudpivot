//! 迁移 `V0381`（成本结算审批流种子）在**真库**上的形态与引擎校验（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_costb32_db \
//!   cargo test --test cost_b3_2_flow_seed_db -- --test-threads=1
//! ```
//!
//! # 这层补的是「静态测试看不见」的三件事
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`definition_is_published_and_discoverable`] | 流程定义 PUBLISHED / 默认 / 不免审；**按 B3-3 将来取流程的那条 SQL 能被选中** |
//! | 2 | [`nodes_are_two_levels_bound_to_roles`] | 4 节点 / 3 边；两级审批；`approval_mode=ANY`（不是全员会签）；审批人绑角色 |
//! | 3 | [`graph_passes_engine_validation_and_hash_matches_rows`] | `FlowGraph::load_graph` 读回来的图 `validate()` **0 问题**；`graph_hash()` == 库里的 `graph_hash` == 迁移里的常量（行 / 快照 / hash 三方对账，即 `flow::maintain` 每日自检的那把尺） |
//!
//! 只读、不造夹具、不改库：本卡是**种子**卡，库里唯一该存在的就是迁移种下的那一条流程。

use nik::prelude::*;
use nik::uuid::Uuid;

const FLOW_CODE: &str = "cost_settle_approve";
const BIZ_TYPE: &str = "COST_SETTLE";
const DEF_ID: &str = "20000000-0000-4000-8000-0000000b0381";
const VERSION_ID: &str = "20000000-0000-4000-8000-0000000b0382";
const FINANCE_MANAGER: &str = "20000000-0000-4000-8000-0000000f0005";
const FINANCE_DIRECTOR: &str = "20000000-0000-4000-8000-0000000f0006";
/// 迁移里写死的 `graph_hash`（sha256 of canonical 快照）。
const EXPECTED_HASH: &str = "8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171";

/// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("B3-2 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap_or_else(|e| panic!("UUID 字面量 {text}: {e}"))
}

// ============================================================================
// ① 定义：PUBLISHED / 默认 / 不免审 / 可被业务桥选中
// ============================================================================

#[tokio::test]
async fn definition_is_published_and_discoverable() {
    if !ensure_db().await {
        return;
    }
    // 与 `purchase_return_bridge::published_flow` 同形状的取流程 SQL：
    // B3-3 提交审批时会这么找流程，种出来的行必须能被选中。
    let row = db()
        .find_first_with_params(
            "SELECT d.id, d.flow_code, d.biz_type, d.status, d.is_default, d.allow_auto_approve, \
                    d.category, d.current_version_id, d.current_version_no, \
                    v.version_status, v.is_current, v.graph_hash, v.node_count, v.edge_count \
               FROM biz_flow_def d \
               JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, d.updated_at DESC",
            vec![
                Value::from(BIZ_TYPE.to_string()),
                Value::from(FLOW_CODE.to_string()),
                Value::from(BIZ_TYPE.to_string()),
            ],
        )
        .await
        .expect("查询流程定义失败");
    let row = row.expect("B3-3 取流程的 SQL 必须能选中 cost_settle_approve（否则提交审批 404）");

    let def_id: Uuid = row.get_typed("id").expect("id");
    assert_eq!(def_id, uuid(DEF_ID), "应命中本卡种的那条流程定义");
    let flow_code: String = row.get_typed("flow_code").expect("flow_code");
    assert_eq!(flow_code, FLOW_CODE);
    let biz_type: String = row.get_typed("biz_type").expect("biz_type");
    assert_eq!(biz_type, BIZ_TYPE);
    let category: Option<String> = row.get_typed("category").expect("category");
    assert_eq!(category.as_deref(), Some("财务管理"));
    let is_default: bool = row.get_typed("is_default").expect("is_default");
    assert!(is_default, "必须是 COST_SETTLE 的默认流程（发起时不指定流程就走它）");
    let auto: bool = row.get_typed("allow_auto_approve").expect("allow_auto_approve");
    assert!(!auto, "结算不许免审直过（设计 §4.2；免审分支没有审批人兜底）");
    let version_no: i32 = row.get_typed("current_version_no").expect("current_version_no");
    assert_eq!(version_no, 1, "首次种子就是 v1");
    let version_id: Uuid = row.get_typed("current_version_id").expect("current_version_id");
    assert_eq!(version_id, uuid(VERSION_ID), "def 的当前版本指针必须指向本卡版本");
    let version_status: String = row.get_typed("version_status").expect("version_status");
    assert_eq!(version_status, "PUBLISHED");
    let is_current: bool = row.get_typed("is_current").expect("is_current");
    assert!(
        is_current,
        "当前版本必须 is_current（否则编辑器里看不到、发起时取不到）"
    );
    let node_count: i32 = row.get_typed("node_count").expect("node_count");
    let edge_count: i32 = row.get_typed("edge_count").expect("edge_count");
    assert_eq!((node_count, edge_count), (4, 3), "4 节点 / 3 边");
}

// ============================================================================
// ② 节点：两级、或签、绑角色
// ============================================================================

#[tokio::test]
async fn nodes_are_two_levels_bound_to_roles() {
    if !ensure_db().await {
        return;
    }
    let rows = db()
        .find_with_params(
            "SELECT node_key, node_name, node_type, assignee_type, approval_mode, empty_assignee, \
                    self_approval, sort_order, timeout_hours, timeout_action \
               FROM biz_flow_node WHERE version_id = ?::uuid ORDER BY sort_order",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("查询节点失败");
    assert_eq!(rows.len(), 4, "节点数必须是 4（开始 / 财务经理 / 财务总监 / 结束）");

    let keys: Vec<String> = rows.iter().map(|r| r.get_typed("node_key").unwrap()).collect();
    assert_eq!(keys, vec!["s", "n_fm", "n_fd", "e"], "节点顺序与 key");

    let names: Vec<String> = rows.iter().map(|r| r.get_typed("node_name").unwrap()).collect();
    assert_eq!(
        names,
        vec!["开始", "财务经理审批", "财务总监审批", "结束"],
        "两级审批：财务经理 → 财务总监"
    );

    for row in rows
        .iter()
        .filter(|r| r.get_typed::<String>("node_type").unwrap() == "APPROVAL")
    {
        let key: String = row.get_typed("node_key").unwrap();
        let assignee_type: Option<String> = row.get_typed("assignee_type").unwrap();
        assert_eq!(assignee_type.as_deref(), Some("ROLE"), "{key} 审批人必须绑角色");
        let mode: Option<String> = row.get_typed("approval_mode").unwrap();
        assert_eq!(
            mode.as_deref(),
            Some("ANY"),
            "{key} 必须是或签 ANY（ALL = 全员会签：一人停用就永久卡单，V0359/V0367 的 P0）"
        );
        let empty: String = row.get_typed("empty_assignee").unwrap();
        assert_eq!(
            empty, "FALLBACK_ADMIN",
            "{key} 角色空了要有兜底（否则节点 0 人 = 死单）"
        );
        let timeout_action: String = row.get_typed("timeout_action").unwrap();
        assert_eq!(
            timeout_action, "REMIND",
            "{key} 超时只能提醒，绝不许自动通过（结算不许被超时静默放行）"
        );
        assert!(
            row.get_typed::<Option<i32>>("timeout_hours").unwrap().is_some(),
            "{key} 要有提醒时限"
        );
    }

    // 审批人 = 角色，顺序固定：n_fm 财务经理、n_fd 财务总监
    let assignees = db()
        .find_with_params(
            "SELECT n.node_key, a.assignee_type, a.ref_id \
               FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid ORDER BY n.sort_order, a.sort_order",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("查询审批人绑定失败");
    assert_eq!(assignees.len(), 2, "两个审批节点各绑一个角色");
    let pairs: Vec<(String, String, Uuid)> = assignees
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("node_key").unwrap(),
                r.get_typed::<String>("assignee_type").unwrap(),
                r.get_typed::<Uuid>("ref_id").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("n_fm".to_string(), "ROLE".to_string(), uuid(FINANCE_MANAGER)),
            ("n_fd".to_string(), "ROLE".to_string(), uuid(FINANCE_DIRECTOR)),
        ],
        "n_fm→FINANCE_MANAGER(…f0005)、n_fd→FINANCE_DIRECTOR(…f0006)"
    );

    // 链路：s→n_fm→n_fd→e
    let edges = db()
        .find_with_params(
            "SELECT e.edge_key, f.node_key AS from_key, t.node_key AS to_key \
               FROM biz_flow_edge e \
               JOIN biz_flow_node f ON f.id = e.from_node_id \
               JOIN biz_flow_node t ON t.id = e.to_node_id \
              WHERE e.version_id = ?::uuid ORDER BY e.edge_key",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("查询边失败");
    let chain: Vec<(String, String)> = edges
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("from_key").unwrap(),
                r.get_typed::<String>("to_key").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        chain,
        vec![
            ("n_fd".to_string(), "e".to_string()),
            ("n_fm".to_string(), "n_fd".to_string()),
            ("s".to_string(), "n_fm".to_string()),
        ],
        "审批链必须 s → n_fm → n_fd → e（按 edge_key 排序后的三元组）"
    );
}

// ============================================================================
// ③ 引擎视角：图能加载、能过校验、hash 三方对账
// ============================================================================

#[tokio::test]
async fn graph_passes_engine_validation_and_hash_matches_rows() {
    if !ensure_db().await {
        return;
    }
    let graph = cloudpivot::business::flow::model::load_graph(uuid(VERSION_ID))
        .await
        .expect("按版本 id 读图失败（引擎提交审批时走的就是这条路径）");

    // ① 引擎校验 0 问题：任何一条 Issue 都意味着「能发起但走不动」或「配错规则」，
    //    典型如 R28（全员会签没有兜底，错误码 65048）。
    let issues = graph.validate();
    assert!(
        issues.is_empty(),
        "种子图必须过引擎校验（否则提交审批/发布即被拦）：{}",
        issues
            .iter()
            .map(|i| format!("[{}] {} ({})", i.rule, i.message, i.code))
            .collect::<Vec<_>>()
            .join(" | ")
    );

    // ② 行 → hash：用仓里同一套 canonical 规则重算
    let recomputed = graph.graph_hash();
    assert_eq!(recomputed, EXPECTED_HASH, "行数据重算的 hash 必须等于迁移里的常量");

    // ③ 库里的 hash 与常量一致（快照没错、列没被改）
    let stored: String = db()
        .scalar_with_params(
            "SELECT graph_hash FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("读 graph_hash 失败（version 行必须存在）");
    assert_eq!(
        stored, EXPECTED_HASH,
        "库里 graph_hash 与常量不一致（手写 hash 与快照分叉）"
    );

    // ④ 快照 shape：4 节点 / 3 边，审批节点 ANY + 角色
    let snapshot = graph.snapshot();
    assert_eq!(snapshot.nodes.len(), 4);
    assert_eq!(snapshot.edges.len(), 3);
    let approvals: Vec<_> = snapshot.nodes.iter().filter(|n| n.node_type == "APPROVAL").collect();
    assert_eq!(approvals.len(), 2, "两级审批");
    for node in approvals {
        assert_eq!(node.approval_mode.as_deref(), Some("ANY"), "{} 必须或签", node.key);
        assert_eq!(node.assignee_type.as_deref(), Some("ROLE"), "{} 必须绑角色", node.key);
        assert_eq!(node.assignees.len(), 1, "{} 恰好绑一个角色", node.key);
    }

    // ⑤ 「不是全员会签」的最终判据（对着引擎解析后的枚举，而不是字符串）
    let all_nodes: Vec<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| n.approval_mode.as_deref() == Some("ALL"))
        .map(|n| n.key.as_str())
        .collect();
    assert!(all_nodes.is_empty(), "种子图里不许有全员会签节点：{all_nodes:?}");
}
