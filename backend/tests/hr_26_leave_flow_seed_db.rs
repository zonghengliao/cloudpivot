//! 迁移 `V0393`（请假审批流种子 `hr_leave`）在**真库**上的形态与引擎校验（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@127.0.0.1:15432/hr26_leave_flow_drill \
//!   cargo test --test hr_26_leave_flow_seed_db -- --test-threads=1
//! ```
//!
//! # 这层补的是「静态测试看不见」的四件事
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`definition_is_published_and_discoverable`] | 定义 PUBLISHED / 默认 / 系统预置 / 不免审；**HR-25 提交审批取流程的那条 SQL 能被选中** |
//! | 2 | [`nodes_route_superior_then_hr_manager_on_long_leave`] | 5 节点 / 5 边；`n_sup` = INITIATOR_SUPERIOR（无枚举行）、`n_hr` = ROLE HR_MANAGER；两节点都 `ANY`（不是全员会签） |
//! | 3 | [`graph_passes_engine_validation_and_hash_matches_rows`] | `load_graph` 读回来的图 `validate()` **0 问题**；`graph_hash()` == 库里 == 迁移常量（三方对账，即 `flow::maintain` 每日自检那把尺） |
//! | 4 | [`long_leave_condition_routes_at_the_three_day_boundary`] | 条件边在**引擎求值器**上真的分流：2.5 天 → 兜底（直属上级终审）、3.0 / 3.5 天 → 人事经理。这条是「≥3 天要复核」的唯一硬证据 |
//!
//! 只读、不造夹具、不改库：本卡是**种子**卡，库里唯一该存在的就是迁移种下的那一条流程。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::{Map, Value as Json};

const FLOW_CODE: &str = "hr_leave";
const BIZ_TYPE: &str = "HR_LEAVE";
const DEF_ID: &str = "20000000-0000-4000-8000-0000000e0393";
const VERSION_ID: &str = "20000000-0000-4000-8000-0000000e0394";
const HR_MANAGER: &str = "20000000-0000-4000-8000-0000000f0016";
/// 迁移里写死的 `graph_hash`（sha256 of canonical 快照）。
const EXPECTED_HASH: &str = "4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97";

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
                    println!("HR-26 已连接验证库");
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
// ① 定义：PUBLISHED / 默认 / 系统预置 / 不免审 / 能被提交审批选中
// ============================================================================

#[tokio::test]
async fn definition_is_published_and_discoverable() {
    if !ensure_db().await {
        return;
    }
    // 与 `hr::leave_handler` 提交审批取流程的 SQL 同形状（biz_type 精确命中优先 → is_default
    // → 更新时间）。种出来的行必须能被它选中，否则提交审批就是 409 + 66033。
    let row = db()
        .find_first_with_params(
            "SELECT d.id, d.flow_code, d.biz_type, d.status, d.is_default, d.is_system, \
                    d.allow_auto_approve, d.category, d.current_version_id, d.current_version_no, \
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
    let row = row.expect("提交审批取流程的 SQL 必须能选中 hr_leave（否则请假单提交即 409）");

    let def_id: Uuid = row.get_typed("id").expect("id");
    assert_eq!(def_id, uuid(DEF_ID), "应命中本卡种的那条流程定义");
    let flow_code: String = row.get_typed("flow_code").expect("flow_code");
    assert_eq!(flow_code, FLOW_CODE);
    let biz_type: String = row.get_typed("biz_type").expect("biz_type");
    assert_eq!(biz_type, BIZ_TYPE);
    let category: Option<String> = row.get_typed("category").expect("category");
    assert_eq!(category.as_deref(), Some("人力资源"));
    let is_default: bool = row.get_typed("is_default").expect("is_default");
    assert!(is_default, "必须是 HR_LEAVE 的默认流程（发起时不指定流程就走它）");
    let is_system: bool = row.get_typed("is_system").expect("is_system");
    assert!(
        is_system,
        "预置流程必须 is_system = TRUE（设计 §3.2 ⑨；客户不能删，P2-6b 的 H6 不变量会判红）"
    );
    let auto: bool = row.get_typed("allow_auto_approve").expect("allow_auto_approve");
    assert!(!auto, "请假不许免审直过（AGENTS 约定 11 的最高危分支）");
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
    assert_eq!((node_count, edge_count), (5, 5), "5 节点 / 5 边");
}

// ============================================================================
// ② 节点：直属上级 → 天数判定 →（≥3 天）人事经理
// ============================================================================

#[tokio::test]
async fn nodes_route_superior_then_hr_manager_on_long_leave() {
    if !ensure_db().await {
        return;
    }
    let rows = db()
        .find_with_params(
            "SELECT node_key, node_name, node_type, assignee_type, approval_mode, \
                    empty_assignee, self_approval, superior_levels, sort_order, \
                    timeout_hours, timeout_action \
               FROM biz_flow_node WHERE version_id = ?::uuid ORDER BY sort_order",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("查询节点失败");
    assert_eq!(
        rows.len(),
        5,
        "节点数必须是 5（开始 / 直属上级 / 天数判定 / 人事经理 / 结束）"
    );

    let keys: Vec<String> = rows.iter().map(|r| r.get_typed("node_key").unwrap()).collect();
    assert_eq!(keys, vec!["s", "n_sup", "c_days", "n_hr", "e"], "节点顺序与 key");
    let names: Vec<String> = rows.iter().map(|r| r.get_typed("node_name").unwrap()).collect();
    assert_eq!(
        names,
        vec!["开始", "直属上级审批", "请假天数判定（≥3 天）", "人事经理复核", "结束"],
        "拓扑：直属上级 →（≥3 天）人事经理 → 结束（设计 §3.2 ⑨）"
    );

    for row in rows
        .iter()
        .filter(|r| r.get_typed::<String>("node_type").unwrap() == "APPROVAL")
    {
        let key: String = row.get_typed("node_key").unwrap();
        let mode: Option<String> = row.get_typed("approval_mode").unwrap();
        assert_eq!(
            mode.as_deref(),
            Some("ANY"),
            "{key} 必须是或签 ANY（ALL = 全员会签：一人停用就永久卡单，V0359/V0367 的 P0）"
        );
        let empty: String = row.get_typed("empty_assignee").unwrap();
        assert_eq!(
            empty, "FALLBACK_ADMIN",
            "{key} 没人可派要有兜底（SKIP 会把「没人」变成自动通过）"
        );
        let timeout_action: String = row.get_typed("timeout_action").unwrap();
        assert_eq!(timeout_action, "REMIND", "{key} 超时只能提醒，绝不许自动通过");
        assert!(
            row.get_typed::<Option<i32>>("timeout_hours").unwrap().is_some(),
            "{key} 要有提醒时限"
        );
        let self_approval: String = row.get_typed("self_approval").unwrap();
        assert_eq!(
            self_approval, "ALLOW",
            "{key} 自审拦截走桥侧（HR-27）；写 FORBID 会让员工自己提的单派发不出来 = 死单"
        );
    }

    // 直属上级：INITIATOR_SUPERIOR + levels=1，**没有**枚举行
    let superior_type: Option<String> = rows
        .iter()
        .find(|r| r.get_typed::<String>("node_key").unwrap() == "n_sup")
        .unwrap()
        .get_typed("assignee_type")
        .unwrap();
    assert_eq!(
        superior_type.as_deref(),
        Some("INITIATOR_SUPERIOR"),
        "直属上级必须随发起人变化（写死角色 / 部门 = 「A 的上级永远是 B」）"
    );

    // 人事经理：绑角色 HR_MANAGER，恰好一行
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
    assert_eq!(assignees.len(), 1, "只有人事经理那一级绑角色（直属上级是不可枚举来源）");
    let node_key: String = assignees[0].get_typed("node_key").unwrap();
    let kind: String = assignees[0].get_typed("assignee_type").unwrap();
    let ref_id: Uuid = assignees[0].get_typed("ref_id").unwrap();
    assert_eq!(
        (node_key.as_str(), kind.as_str(), ref_id),
        ("n_hr", "ROLE", uuid(HR_MANAGER)),
        "n_hr → HR_MANAGER（…f0016）"
    );

    // 链路：s→n_sup→c_days→{e | n_hr→e}
    let edges = db()
        .find_with_params(
            "SELECT e.edge_key, f.node_key AS from_key, t.node_key AS to_key, \
                    e.condition_expr IS NOT NULL AS has_condition, e.is_default \
               FROM biz_flow_edge e \
               JOIN biz_flow_node f ON f.id = e.from_node_id \
               JOIN biz_flow_node t ON t.id = e.to_node_id \
              WHERE e.version_id = ?::uuid ORDER BY e.edge_key",
            vec![Value::from(uuid(VERSION_ID))],
        )
        .await
        .expect("查询边失败");
    let chain: Vec<(String, String, String)> = edges
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("edge_key").unwrap(),
                r.get_typed::<String>("from_key").unwrap(),
                r.get_typed::<String>("to_key").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        chain,
        vec![
            ("e_hr_end".to_string(), "n_hr".to_string(), "e".to_string()),
            ("e_long".to_string(), "c_days".to_string(), "n_hr".to_string()),
            ("e_short".to_string(), "c_days".to_string(), "e".to_string()),
            ("e_start".to_string(), "s".to_string(), "n_sup".to_string()),
            ("e_sup_days".to_string(), "n_sup".to_string(), "c_days".to_string()),
        ],
        "骨架必须 s → n_sup → c_days →（e_long）n_hr → e /（e_short）e"
    );
    let defaults: Vec<&_> = edges
        .iter()
        .filter(|r| r.get_typed::<bool>("is_default").unwrap())
        .collect();
    assert_eq!(defaults.len(), 1, "条件节点必须恰有 1 条兜底边（R13）");
    assert_eq!(
        defaults[0].get_typed::<String>("edge_key").unwrap(),
        "e_short",
        "兜底边 =「请假 < 3 天 → 直属上级终审」"
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

    let issues = graph.validate();
    assert!(
        issues.is_empty(),
        "种子图必须过引擎校验（否则发布 / 提交审批即被拦）：{}",
        issues
            .iter()
            .map(|i| format!("[{}] {} ({})", i.rule, i.message, i.code))
            .collect::<Vec<_>>()
            .join(" | ")
    );

    let recomputed = graph.graph_hash();
    assert_eq!(recomputed, EXPECTED_HASH, "行数据重算的 hash 必须等于迁移里的常量");

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

    let snapshot = graph.snapshot();
    assert_eq!(snapshot.nodes.len(), 5);
    assert_eq!(snapshot.edges.len(), 5);
    let approvals: Vec<_> = snapshot.nodes.iter().filter(|n| n.node_type == "APPROVAL").collect();
    assert_eq!(approvals.len(), 2, "两级：直属上级 + 人事经理");
    for node in approvals {
        assert_eq!(node.approval_mode.as_deref(), Some("ANY"), "{} 必须或签", node.key);
        assert_eq!(
            node.assignees.len(),
            usize::from(node.assignee_type.as_deref() == Some("ROLE"))
        );
    }
}

// ============================================================================
// ④ 条件边在引擎求值器上真的分流（「≥3 天要复核」的唯一硬证据）
// ============================================================================

#[tokio::test]
async fn long_leave_condition_routes_at_the_three_day_boundary() {
    if !ensure_db().await {
        return;
    }
    let graph = cloudpivot::business::flow::model::load_graph(uuid(VERSION_ID))
        .await
        .expect("按版本 id 读图失败");
    let snapshot = graph.snapshot();
    let long_edge = snapshot
        .edges
        .iter()
        .find(|edge| edge.key == "e_long")
        .expect("必须有长假条件边 e_long");
    let raw = long_edge.condition.clone().expect("e_long 必须带条件");
    let condition =
        cloudpivot::business::flow::graph::parse_condition(&raw).expect("条件 DSL 必须合法（否则提交审批 500）");

    for (days, expected) in [
        (0.5_f64, false),
        (2.5, false),
        (2.9, false),
        (3.0, true),
        (3.5, true),
        (30.0, true),
    ] {
        let mut form = Map::new();
        // 与 `leave_handler::leave_form_data` 同形状：数值镜像（JSON number）
        form.insert(
            "duration_days_num".to_string(),
            serde_json::Number::from_f64(days).map(Json::Number).unwrap(),
        );
        form.insert("duration_days".to_string(), Json::String(format!("{days}")));
        let context = cloudpivot::business::flow::graph::EvalContext {
            form,
            ..Default::default()
        };
        let hit = cloudpivot::business::flow::graph::eval(&condition, &context)
            .unwrap_or_else(|e| panic!("{days} 天求值失败：{e}"));
        assert_eq!(hit, expected, "{days} 天 → 走人事经理复核应为 {expected}（阈值 gte 3）");
    }

    // 反证：只给字符串形态（旧口径）时求值必须报错，而不是「静默 false」——
    // 这正是本卡必须补数值镜像的原因（引擎侧会把它当「条件不成立」走兜底边）。
    let mut form = Map::new();
    form.insert("duration_days".to_string(), Json::String("10".to_string()));
    let context = cloudpivot::business::flow::graph::EvalContext {
        form,
        ..Default::default()
    };
    let error = cloudpivot::business::flow::graph::eval(&condition, &context)
        .expect_err("只有字符串 duration_days 时求值必须报 Missing / TypeMismatch，不能静默为 false");
    println!("字符串形态的求值错误（预期，说明为什么要有数值镜像）：{error}");
}
