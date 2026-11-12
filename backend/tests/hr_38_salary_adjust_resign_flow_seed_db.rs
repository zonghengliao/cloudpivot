//! 迁移 `V0394` / `V0395`（薪资单 / 调薪 / 离职 三套审批流种子）在**真库**上的形态与引擎校验
//! （`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@127.0.0.1:15432/hr38_drill \
//!   cargo test --test hr_38_salary_adjust_resign_flow_seed_db -- --test-threads=1
//! ```
//!
//! # 这层补的是「静态测试看不见」的四件事
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`three_definitions_are_published_and_discoverable`] | 三套定义都 PUBLISHED / 默认 / 系统预置 / 不免审；**提交审批取流程的那条 SQL 能被选中** |
//! | 2 | [`salary_flow_binds_hr_and_finance_and_escalates_on_500k`] | 6 节点 / 6 边；`n_fin` = COUNTERSIGN + ALL + 两个角色；`n_hr`/`n_gm` = ANY + ROLE；条件边 `gte 500000` |
//! | 3 | [`adjust_and_resign_bind_roles_without_enumerating_the_superior`] | 调薪 5/4、离职 6/6；`n_sup` = INITIATOR_SUPERIOR **无枚举行**；离职条件边 `is_key_position eq true` |
//! | 4 | [`graphs_pass_engine_validation_and_hash_matches_rows`] | `load_graph` 读回来的图 `validate()` **0 问题**；`graph_hash()` == 库里 == 迁移常量（三方对账） |
//! | 5 | [`conditions_route_at_the_threshold`] | 条件边在**引擎求值器**上真的分流：49.99 万 → 兜底；50 万 / 80 万 → 总经理；关键岗位 true → 总经理、false → 兜底 |
//!
//! 只读、不造夹具、不改库：本卡是**种子**卡，库里唯一该存在的就是迁移种下的那三条流程。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::{Map, Value as Json};

const HR_MANAGER: &str = "20000000-0000-4000-8000-0000000f0016";
const FINANCE_MANAGER: &str = "20000000-0000-4000-8000-0000000f0005";
const FINANCE_DIRECTOR: &str = "20000000-0000-4000-8000-0000000f0006";
const GENERAL_MANAGER: &str = "20000000-0000-4000-8000-0000000f0003";

/// 三套流程的固定 id / 常量（与迁移逐字对应）。
struct Seeded {
    flow_code: &'static str,
    biz_type: &'static str,
    def_id: &'static str,
    version_id: &'static str,
    hash: &'static str,
    node_count: i32,
    edge_count: i32,
}

const SALARY: Seeded = Seeded {
    flow_code: "hr_salary",
    biz_type: "HR_SALARY",
    def_id: "20000000-0000-4000-8000-0000000e1401",
    version_id: "20000000-0000-4000-8000-0000000e1402",
    hash: "0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0",
    node_count: 6,
    edge_count: 6,
};

const ADJUST: Seeded = Seeded {
    flow_code: "hr_salary_adjust",
    biz_type: "HR_SALARY_ADJUST",
    def_id: "20000000-0000-4000-8000-0000000e1501",
    version_id: "20000000-0000-4000-8000-0000000e1502",
    hash: "a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171",
    node_count: 5,
    edge_count: 4,
};

const RESIGN: Seeded = Seeded {
    flow_code: "hr_employee_resign",
    biz_type: "HR_EMPLOYEE_RESIGN",
    def_id: "20000000-0000-4000-8000-0000000e1531",
    version_id: "20000000-0000-4000-8000-0000000e1532",
    hash: "ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496",
    node_count: 6,
    edge_count: 6,
};

const ALL_FLOWS: [&Seeded; 3] = [&SALARY, &ADJUST, &RESIGN];

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
                    println!("HR-38 已连接验证库");
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
// ① 三套定义：PUBLISHED / 默认 / 系统预置 / 不免审 / 能被提交审批选中
// ============================================================================

#[tokio::test]
async fn three_definitions_are_published_and_discoverable() {
    if !ensure_db().await {
        return;
    }
    for seeded in ALL_FLOWS {
        // 与「提交审批取流程」的 SQL 同形状（biz_type 精确命中优先 → is_default → 更新时间）。
        // 种出来的行必须能被它选中，否则提交审批就是 409 + 66033。
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
                    Value::from(seeded.biz_type.to_string()),
                    Value::from(seeded.flow_code.to_string()),
                    Value::from(seeded.biz_type.to_string()),
                ],
            )
            .await
            .expect("查询流程定义失败");
        let row = row.unwrap_or_else(|| {
            panic!(
                "提交审批取流程的 SQL 必须能选中 {}（否则这类单据提交即 409）",
                seeded.flow_code
            )
        });

        let def_id: Uuid = row.get_typed("id").expect("id");
        assert_eq!(def_id, uuid(seeded.def_id), "{} 应命中本卡种的定义", seeded.flow_code);
        let flow_code: String = row.get_typed("flow_code").expect("flow_code");
        assert_eq!(flow_code, seeded.flow_code);
        let biz_type: String = row.get_typed("biz_type").expect("biz_type");
        assert_eq!(biz_type, seeded.biz_type);
        let category: Option<String> = row.get_typed("category").expect("category");
        assert_eq!(category.as_deref(), Some("人力资源"));
        assert!(
            row.get_typed::<bool>("is_default").expect("is_default"),
            "必须是默认流程"
        );
        assert!(
            row.get_typed::<bool>("is_system").expect("is_system"),
            "预置流程必须 is_system = TRUE（客户不能删，P2-6b 的 H6 判红）"
        );
        assert!(
            !row.get_typed::<bool>("allow_auto_approve").expect("allow_auto_approve"),
            "不许免审直过（AGENTS 约定 11 的最高危分支）"
        );
        let version_no: i32 = row.get_typed("current_version_no").expect("current_version_no");
        assert_eq!(version_no, 1, "首次种子就是 v1");
        let version_id: Uuid = row.get_typed("current_version_id").expect("current_version_id");
        assert_eq!(version_id, uuid(seeded.version_id), "def 指针必须指向本卡版本");
        assert_eq!(
            row.get_typed::<String>("version_status").expect("version_status"),
            "PUBLISHED"
        );
        assert!(
            row.get_typed::<bool>("is_current").expect("is_current"),
            "当前版本必须 is_current（否则编辑器/发起都取不到）"
        );
        assert_eq!(
            row.get_typed::<String>("graph_hash").expect("graph_hash"),
            seeded.hash,
            "{} 落库 hash 必须等于迁移常量",
            seeded.flow_code
        );
        assert_eq!(
            (
                row.get_typed::<i32>("node_count").expect("node_count"),
                row.get_typed::<i32>("edge_count").expect("edge_count")
            ),
            (seeded.node_count, seeded.edge_count),
            "{} 的节点/边数",
            seeded.flow_code
        );
    }
}

// ============================================================================
// ② 薪资流程形状：人事经理 → 财务会签（两角色）→（≥50 万）总经理
// ============================================================================

#[tokio::test]
async fn salary_flow_binds_hr_and_finance_and_escalates_on_500k() {
    if !ensure_db().await {
        return;
    }
    let rows = db()
        .find_with_params(
            "SELECT node_key, node_name, node_type, assignee_type, approval_mode, \
                    empty_assignee, self_approval, timeout_hours, timeout_action \
               FROM biz_flow_node WHERE version_id = ?::uuid ORDER BY sort_order",
            vec![Value::from(uuid(SALARY.version_id))],
        )
        .await
        .expect("查询节点失败");
    let keys: Vec<String> = rows.iter().map(|r| r.get_typed("node_key").unwrap()).collect();
    assert_eq!(
        keys,
        vec!["s", "n_hr", "n_fin", "c_amt", "n_gm", "e"],
        "6 节点顺序（设计 §3.2 ⑨：人事经理 → 财务会签 →（≥50 万）总经理）"
    );
    let names: Vec<String> = rows.iter().map(|r| r.get_typed("node_name").unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "开始",
            "人事经理审核",
            "财务会签",
            "应发合计判定（≥50 万）",
            "总经理审批",
            "结束"
        ]
    );

    // 两个 APPROVAL：显式 ANY + 兜底 + 超时只提醒
    let approvals: Vec<&_> = rows
        .iter()
        .filter(|r| r.get_typed::<String>("node_type").unwrap() == "APPROVAL")
        .collect();
    assert_eq!(approvals.len(), 2, "两级单人审批：人事经理 + 总经理");
    for row in approvals {
        let key: String = row.get_typed("node_key").unwrap();
        assert_eq!(
            row.get_typed::<Option<String>>("approval_mode").unwrap().as_deref(),
            Some("ANY"),
            "{key} 必须是或签 ANY（APPROVAL + ALL = 全员会签，一人停用即卡单）"
        );
        assert_eq!(row.get_typed::<String>("empty_assignee").unwrap(), "FALLBACK_ADMIN");
        assert_eq!(row.get_typed::<String>("timeout_action").unwrap(), "REMIND");
        assert_eq!(row.get_typed::<String>("self_approval").unwrap(), "ALLOW");
        assert!(row.get_typed::<Option<i32>>("timeout_hours").unwrap().is_some());
    }

    // 会签节点：COUNTERSIGN + ALL + 兜底 + 有超时
    let fin = rows
        .iter()
        .find(|r| r.get_typed::<String>("node_key").unwrap() == "n_fin")
        .expect("财务会签");
    assert_eq!(fin.get_typed::<String>("node_type").unwrap(), "COUNTERSIGN");
    assert_eq!(
        fin.get_typed::<Option<String>>("approval_mode").unwrap().as_deref(),
        Some("ALL"),
        "财务会签 = 全员签"
    );
    assert_eq!(fin.get_typed::<String>("empty_assignee").unwrap(), "FALLBACK_ADMIN");
    assert!(
        fin.get_typed::<Option<i32>>("timeout_hours").unwrap().is_some(),
        "ALL 节点必须有超时（R28：没有超时/兜底的全员会签会永久卡死）"
    );

    // 审批人绑定：4 行（人事经理 / 财务经理 / 财务总监 / 总经理）
    let assignees = db()
        .find_with_params(
            "SELECT n.node_key, a.assignee_type, a.ref_id \
               FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid ORDER BY n.sort_order, a.sort_order",
            vec![Value::from(uuid(SALARY.version_id))],
        )
        .await
        .expect("查询审批人绑定失败");
    let binding: Vec<(String, Uuid)> = assignees
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("node_key").unwrap(),
                r.get_typed::<Uuid>("ref_id").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        binding,
        vec![
            ("n_hr".to_string(), uuid(HR_MANAGER)),
            ("n_fin".to_string(), uuid(FINANCE_MANAGER)),
            ("n_fin".to_string(), uuid(FINANCE_DIRECTOR)),
            ("n_gm".to_string(), uuid(GENERAL_MANAGER)),
        ],
        "人事经理 / 财务经理 + 财务总监（会签） / 总经理，全是角色（换人不用改流程）"
    );

    // 边：6 条，条件边 gte 500000
    let edges = db()
        .find_with_params(
            "SELECT e.edge_key, f.node_key AS from_key, t.node_key AS to_key, e.is_default \
               FROM biz_flow_edge e \
               JOIN biz_flow_node f ON f.id = e.from_node_id \
               JOIN biz_flow_node t ON t.id = e.to_node_id \
              WHERE e.version_id = ?::uuid ORDER BY e.edge_key",
            vec![Value::from(uuid(SALARY.version_id))],
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
            ("e_big".to_string(), "c_amt".to_string(), "n_gm".to_string()),
            ("e_fin_amt".to_string(), "n_fin".to_string(), "c_amt".to_string()),
            ("e_gm_end".to_string(), "n_gm".to_string(), "e".to_string()),
            ("e_hr_fin".to_string(), "n_hr".to_string(), "n_fin".to_string()),
            ("e_small".to_string(), "c_amt".to_string(), "e".to_string()),
            ("e_start".to_string(), "s".to_string(), "n_hr".to_string()),
        ],
        "骨架：s → n_hr → n_fin → c_amt →（e_big）n_gm → e /（e_small）e"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|r| r.get_typed::<bool>("is_default").unwrap())
            .count(),
        1,
        "条件节点必须恰有 1 条兜底边（R13）"
    );
}

// ============================================================================
// ③ 调薪 / 离职：直属上级不落枚举行，角色绑定齐
// ============================================================================

#[tokio::test]
async fn adjust_and_resign_bind_roles_without_enumerating_the_superior() {
    if !ensure_db().await {
        return;
    }
    // ---- 调薪：5 节点 / 4 边，三签 ----
    let adjust_nodes = db()
        .find_with_params(
            "SELECT node_key, node_type, assignee_type, approval_mode \
               FROM biz_flow_node WHERE version_id = ?::uuid ORDER BY sort_order",
            vec![Value::from(uuid(ADJUST.version_id))],
        )
        .await
        .expect("查询调薪节点失败");
    let adjust_keys: Vec<String> = adjust_nodes.iter().map(|r| r.get_typed("node_key").unwrap()).collect();
    assert_eq!(adjust_keys, vec!["s", "n_sup", "n_hr", "n_fin", "e"]);
    assert_eq!(
        adjust_nodes
            .iter()
            .filter(|r| r.get_typed::<String>("node_type").unwrap() == "APPROVAL")
            .count(),
        3,
        "调薪三签：直属上级 → 人事经理 → 财务经理"
    );
    assert_eq!(
        adjust_nodes
            .iter()
            .filter(|r| r.get_typed::<String>("node_type").unwrap() == "COUNTERSIGN")
            .count(),
        0,
        "调薪不需要会签节点"
    );
    let adjust_sup = adjust_nodes
        .iter()
        .find(|r| r.get_typed::<String>("node_key").unwrap() == "n_sup")
        .expect("n_sup");
    assert_eq!(
        adjust_sup
            .get_typed::<Option<String>>("assignee_type")
            .unwrap()
            .as_deref(),
        Some("INITIATOR_SUPERIOR"),
        "直属上级随发起人变化（写死角色 = 「A 的上级永远是 B」）"
    );
    let adjust_binding = db()
        .find_with_params(
            "SELECT n.node_key, a.ref_id FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid ORDER BY n.sort_order, a.sort_order",
            vec![Value::from(uuid(ADJUST.version_id))],
        )
        .await
        .expect("查询调薪审批人失败");
    let adjust_pairs: Vec<(String, Uuid)> = adjust_binding
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("node_key").unwrap(),
                r.get_typed::<Uuid>("ref_id").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        adjust_pairs,
        vec![
            ("n_hr".to_string(), uuid(HR_MANAGER)),
            ("n_fin".to_string(), uuid(FINANCE_MANAGER)),
        ],
        "调薪只绑人事经理 + 财务经理；直属上级不许有枚举行"
    );

    // ---- 离职：6 节点 / 6 边，关键岗位分叉 ----
    let resign_nodes = db()
        .find_with_params(
            "SELECT node_key, node_type, assignee_type FROM biz_flow_node \
              WHERE version_id = ?::uuid ORDER BY sort_order",
            vec![Value::from(uuid(RESIGN.version_id))],
        )
        .await
        .expect("查询离职节点失败");
    let resign_keys: Vec<String> = resign_nodes.iter().map(|r| r.get_typed("node_key").unwrap()).collect();
    assert_eq!(resign_keys, vec!["s", "n_sup", "n_hr", "c_key", "n_gm", "e"]);
    assert_eq!(
        resign_nodes
            .iter()
            .filter(|r| r.get_typed::<String>("node_key").unwrap() == "c_key")
            .count(),
        1,
        "离职必须有「关键岗位判定」条件节点"
    );
    let resign_pairs = db()
        .find_with_params(
            "SELECT n.node_key, a.ref_id FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid ORDER BY n.sort_order, a.sort_order",
            vec![Value::from(uuid(RESIGN.version_id))],
        )
        .await
        .expect("查询离职审批人失败")
        .iter()
        .map(|r| {
            (
                r.get_typed::<String>("node_key").unwrap(),
                r.get_typed::<Uuid>("ref_id").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resign_pairs,
        vec![
            ("n_hr".to_string(), uuid(HR_MANAGER)),
            ("n_gm".to_string(), uuid(GENERAL_MANAGER)),
        ],
        "离职绑人事经理 + 总经理；直属上级不许有枚举行"
    );

    // 两套流程的节点里都不许出现 APPROVAL + ALL（角色型全员会签）
    for seeded in [&ADJUST, &RESIGN] {
        let bad: i64 = db()
            .scalar_with_params(
                "SELECT count(*)::bigint AS n FROM biz_flow_node \
                  WHERE version_id = ?::uuid AND node_type IN ('APPROVAL','COUNTERSIGN') \
                    AND (approval_mode = 'ALL' OR approval_mode IS NULL)",
                vec![Value::from(uuid(seeded.version_id))],
            )
            .await
            .expect("统计全员会签节点失败");
        assert_eq!(bad, 0, "{} 有全员会签 / 未落 approval_mode 的节点", seeded.flow_code);
    }
}

// ============================================================================
// ④ 引擎视角：图能加载、能过校验、hash 三方对账
// ============================================================================

#[tokio::test]
async fn graphs_pass_engine_validation_and_hash_matches_rows() {
    if !ensure_db().await {
        return;
    }
    for seeded in ALL_FLOWS {
        let graph = cloudpivot::business::flow::model::load_graph(uuid(seeded.version_id))
            .await
            .unwrap_or_else(|e| panic!("{} 按版本 id 读图失败：{e}", seeded.flow_code));
        let issues = graph.validate();
        assert!(
            issues.is_empty(),
            "{} 的种子图必须过引擎校验（否则发布/提交审批即被拦）：{}",
            seeded.flow_code,
            issues
                .iter()
                .map(|i| format!("[{}] {} ({})", i.rule, i.message, i.code))
                .collect::<Vec<_>>()
                .join(" | ")
        );

        assert_eq!(
            graph.graph_hash(),
            seeded.hash,
            "{} 行数据重算的 hash 必须等于迁移常量",
            seeded.flow_code
        );
        let stored: String = db()
            .scalar_with_params(
                "SELECT graph_hash FROM biz_flow_def_version WHERE id = ?::uuid",
                vec![Value::from(uuid(seeded.version_id))],
            )
            .await
            .expect("读 graph_hash 失败");
        assert_eq!(stored, seeded.hash, "{} 库里 hash 与常量不一致", seeded.flow_code);

        let snapshot = graph.snapshot();
        assert_eq!(snapshot.nodes.len() as i32, seeded.node_count);
        assert_eq!(snapshot.edges.len() as i32, seeded.edge_count);
    }
}

// ============================================================================
// ⑤ 条件边在引擎求值器上真的分流（两条金额/岗位分级的唯一硬证据）
// ============================================================================

async fn condition_of(version_id: &str, edge_key: &str) -> cloudpivot::business::flow::graph::Condition {
    let graph = cloudpivot::business::flow::model::load_graph(uuid(version_id))
        .await
        .expect("按版本 id 读图失败");
    let snapshot = graph.snapshot();
    let edge = snapshot
        .edges
        .iter()
        .find(|edge| edge.key == edge_key)
        .unwrap_or_else(|| panic!("图里没有边 {edge_key}"));
    let raw = edge
        .condition
        .clone()
        .unwrap_or_else(|| panic!("{edge_key} 必须带条件（R13）"));
    cloudpivot::business::flow::graph::parse_condition(&raw)
        .unwrap_or_else(|e| panic!("{edge_key} 条件 DSL 不合法：{e}"))
}

#[tokio::test]
async fn conditions_route_at_the_threshold() {
    if !ensure_db().await {
        return;
    }
    // ---- 薪资：应发 ≥ 50 万 → 总经理 ----
    let salary_condition = condition_of(SALARY.version_id, "e_big").await;
    for (amount, expected) in [
        (0.0_f64, false),
        (100_000.0, false),
        (499_999.99, false),
        (500_000.0, true),
        (800_000.0, true),
    ] {
        let mut form = Map::new();
        // 与提交审批冻结 form_data 的契约同形状：数值镜像（JSON number）
        form.insert(
            "gross_total_num".to_string(),
            serde_json::Number::from_f64(amount).map(Json::Number).unwrap(),
        );
        let context = cloudpivot::business::flow::graph::EvalContext {
            form,
            ..Default::default()
        };
        let hit = cloudpivot::business::flow::graph::eval(&salary_condition, &context)
            .unwrap_or_else(|e| panic!("应发 {amount} 求值失败：{e}"));
        assert_eq!(
            hit, expected,
            "应发 {amount} → 走总经理审批应为 {expected}（阈值 gte 500000）"
        );
    }

    // ---- 离职：关键岗位 → 总经理 ----
    let resign_condition = condition_of(RESIGN.version_id, "e_key").await;
    for (flag, expected) in [(true, true), (false, false)] {
        let mut form = Map::new();
        form.insert("is_key_position".to_string(), Json::Bool(flag));
        let context = cloudpivot::business::flow::graph::EvalContext {
            form,
            ..Default::default()
        };
        let hit = cloudpivot::business::flow::graph::eval(&resign_condition, &context)
            .unwrap_or_else(|e| panic!("关键岗位={flag} 求值失败：{e}"));
        assert_eq!(hit, expected, "is_key_position={flag} → 总经理审批应为 {expected}");
    }

    // 反证：字符串形态 "true" 与布尔 true 不相等 ⇒ 静默跳兜底（关键岗位离职没人总经理签字）。
    // 这条断言把「提交审批必须冻结成 JSON boolean」的契约钉死在引擎侧。
    let mut form = Map::new();
    form.insert("is_key_position".to_string(), Json::String("true".to_string()));
    let context = cloudpivot::business::flow::graph::EvalContext {
        form,
        ..Default::default()
    };
    assert!(
        !cloudpivot::business::flow::graph::eval(&resign_condition, &context).expect("字符串不报错，只是不相等"),
        "字符串 \"true\" 会静默绕过总经理审批"
    );
}
