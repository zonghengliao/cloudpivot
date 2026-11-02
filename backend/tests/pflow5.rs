//! P-FLOW-5 端到端验收（**真库**：库名必须含 `pflow5` 或 `cloudpivot_test`）。
//!
//! 设计依据：卡面 t_84fbfea1「三、单测 ≥8 条」+「四、验收」。
//!
//! | 用例 | 卡面 | 断言 |
//! |---|---|---|
//! | `templates_grouped_by_category` | ① | 按 `category` 分组；采购管理组排在任务组之前；未知分类排最后 |
//! | `templates_marks_system_defs` | ② | `is_system` 一 true 一 false，模板库如实下发 |
//! | `templates_usage_count_from_instances` | ③ | 发起 2 单 → `usage_count = 2`、`last_used_at` 非空 |
//! | `templates_summary_from_condition_labels` | ① 补充 | 卡片说明取条件分流标签（不是别的字段） |
//! | `duplicate_creates_draft_copy_with_graph` | ④ | 副本 `DRAFT` / `is_system=false` / `is_default=false` / 节点边完整 |
//! | `duplicate_does_not_copy_instances` | 红线 4 | 副本名下 0 条 `biz_flow_instance`，原模板实例数不变 |
//! | `duplicate_conflict_on_existing_code` | ⑤ | `409 + 10003`，且不产生半成品定义 |
//! | `offline_default_leaves_biz_type_without_default` | ⑥ | 停用默认流程 → 该 biz_type 再也解析不到默认流程 |
//! | `offline_non_default_keeps_other_default` | ⑦ | 停用非默认 → 默认流程不受影响 |
//! | `enable_restores_default_after_offline` | ⑤ 验收 | 停用 → 启用：默认流程回来、版本数不变、重复启用 409 |
//! | `enable_refuses_draft_definitions` | 补充 | 草稿不能靠「启用」绕过发布校验（409 + 状态不变） |
//! | `read_only_graph_endpoint_returns_nodes_edges` | ⑧ | `version_detail` 返回完整节点 + 连线（只读复用） |
//!
//! # 连库约定
//!
//! 只连 `PFLOW5_DB_URL` 指向的库，且**库名必须含 `pflow5` 或 `cloudpivot_test`**
//! （本套会造真实流程定义与审批实例）。跑法：
//!
//! ```bash
//! PFLOW5_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test' \
//!   cargo test --test pflow5 -- --test-threads=1 --nocapture
//! ```

use axum::Json;
use axum::extract::Path;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::flow::{dto as flow_dto, handler as flow_handler, model as flow_model};
use cloudpivot::shared::error_code;

// ==================== 连库 ====================

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("PFLOW5_DB_URL") else {
                println!("SKIP: 未设置 PFLOW5_DB_URL（P-FLOW-5 端到端验收需要真库）");
                return false;
            };
            assert!(
                url.contains("pflow5") || url.contains("/cloudpivot_test"),
                "PFLOW5_DB_URL 必须指向 cloudpivot_test_pflow5 或 cloudpivot_test（当前：{url}）"
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
                        println!("[pflow5] 已连接 {url}");
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

fn claims(user: Uuid) -> Extension<AuthClaims> {
    Extension(AuthClaims(TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some("pflow5-op".to_string()),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec![
            "business:flow:read".to_string(),
            "business:flow:create".to_string(),
            "business:flow:update".to_string(),
            "business:flow:publish".to_string(),
            "business:flow:delete".to_string(),
            "business:flow:instance:create".to_string(),
        ],
        token_type: Some(TokenType::Access),
        exp: 4_000_000_000,
        iat: 1_700_000_000,
        extra: std::collections::HashMap::new(),
    }))
}

/// 短随机尾巴（同毫秒并发建流程也不会撞编码）。
fn tail() -> String {
    Uuid::now_v7().simple().to_string()[24..32].to_string()
}

// ==================== 夹具 ====================

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
                Value::Text(format!("pflow5-{tag}-{}", tail())),
                Value::Text(format!("PFLOW5 {tag} 操作人")),
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
        claims(self.operator)
    }

    fn prefix(&self) -> String {
        format!("pflow5_{}_", self.tag.to_ascii_lowercase())
    }

    /// 清掉本用例造的**数据**（按流程编码前缀，先删实例再删定义）。
    ///
    /// 只清数据、**不动夹具用户**：用例开头就是「建夹具 → 清残留 → 造数据」，
    /// 若这里顺手 `DELETE FROM sys_user WHERE id = 本夹具操作人`，等于把刚建好的
    /// 发起人删了 —— 之后 `create_instance` 取发起人快照必然 401 / `20005`。
    /// 夹具用户的生命周期交给 [`Self::cleanup`]（用例末尾）统一回收。
    async fn reset(&self) {
        let like = Value::Text(format!("{}%", self.prefix()));
        for sql in [
            "DELETE FROM biz_flow_task WHERE instance_id IN ( \
                 SELECT i.id FROM biz_flow_instance i \
                   JOIN biz_flow_def d ON d.id = i.def_id WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN ( \
                 SELECT i.id FROM biz_flow_instance i \
                   JOIN biz_flow_def d ON d.id = i.def_id WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_instance WHERE def_id IN ( \
                 SELECT id FROM biz_flow_def WHERE flow_code LIKE ? )",
            "DELETE FROM biz_flow_edge WHERE version_id IN ( \
                 SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                  WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_node WHERE version_id IN ( \
                 SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
                  WHERE d.flow_code LIKE ? )",
            "DELETE FROM biz_flow_def_version WHERE def_id IN ( \
                 SELECT id FROM biz_flow_def WHERE flow_code LIKE ? )",
            "DELETE FROM biz_flow_def WHERE flow_code LIKE ?",
        ] {
            let _ = db().execute_with_params(sql, vec![like.clone()]).await;
        }
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_task WHERE owner_id = ?::uuid",
                vec![Value::from(&self.operator)],
            )
            .await;
    }

    /// 用例收尾：清数据 + 删夹具用户（不留垃圾账号在测试库里）。
    async fn cleanup(&self) {
        self.reset().await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user WHERE id = ?::uuid",
                vec![Value::from(&self.operator)],
            )
            .await;
    }
}

// ==================== 建流程 / 发起 ====================

/// 建一个流程（`handler::create` 会自带 START→END 的合法初始图），可选发布。
///
/// 返回 `(def_id, version_id)`。
async fn make_def(
    f: &Fixture,
    suffix: &str,
    biz_type: &str,
    category: &str,
    is_default: bool,
    publish: bool,
) -> (Uuid, Uuid) {
    let flow_code = format!("{}{}_{}", f.prefix(), suffix, tail());
    let req = flow_dto::DefCreateReq {
        flow_code,
        flow_name: format!("PFLOW5 {suffix}"),
        biz_type: biz_type.to_string(),
        category: Some(category.to_string()),
        description: Some(format!("PFLOW5 夹具：{suffix}。第二句不该出现在摘要里。")),
        is_default: Some(is_default),
        allow_auto_approve: Some(false),
    };
    let created = flow_handler::create(f.ext(), Json(req))
        .await
        .unwrap_or_else(|error| panic!("建流程失败：{} {}", error.code, error.msg))
        .data
        .expect("建流程必须返回回执");
    let def_id = Uuid::parse_str(&created.id).expect("def id");
    let version_id = Uuid::parse_str(&created.version_id).expect("version id");
    if publish {
        flow_handler::publish(f.ext(), Path(def_id.to_string()))
            .await
            .unwrap_or_else(|error| panic!("发布失败：{} {}", error.code, error.msg));
    }
    (def_id, version_id)
}

/// 发起一张审批单。
async fn start_instance(f: &Fixture, def_id: Uuid, title: &str) -> Uuid {
    let req = flow_dto::InstanceCreateReq {
        def_id: Some(def_id.to_string()),
        flow_code: None,
        biz_type: None,
        biz_id: None,
        biz_no: Some(format!("PFLOW5-{}", tail())),
        title: title.to_string(),
        form_data: Some(serde_json::json!({})),
    };
    let data = flow_handler::create_instance(f.ext(), Json(req))
        .await
        .unwrap_or_else(|error| panic!("发起实例失败：{} {}", error.code, error.msg))
        .data
        .expect("发起必须返回回执");
    Uuid::parse_str(&data.instance_id).expect("instance id")
}

// ==================== 读模板库 ====================

/// 拉一次模板库，返回 `(groups, total)`。
async fn load_templates() -> (Vec<flow_dto::TemplateGroupRes>, u64) {
    let res = flow_handler::templates()
        .await
        .expect("模板库接口必须成功")
        .data
        .expect("模板库必须返回数据");
    (res.groups, res.total)
}

/// 在模板库结果里按 id 找条目。
fn find_item(
    groups: &[flow_dto::TemplateGroupRes],
    def_id: Uuid,
) -> Option<flow_dto::TemplateItemRes> {
    let target = def_id.to_string();
    groups
        .iter()
        .flat_map(|group| group.items.iter())
        .find(|item| item.id == target)
        .cloned()
}

async fn count_rows(sql: &str, id: Uuid) -> i64 {
    let rows = db()
        .find_with_params(sql, vec![Value::from(&id)])
        .await
        .expect("计数查询");
    rows.first()
        .map(|row| row.get_typed::<i64>("total").expect("total 列"))
        .unwrap_or(0)
}

// ==================== ① 分组 ====================

#[tokio::test]
async fn templates_grouped_by_category() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("group").await;
    f.reset().await;

    let (buy_id, _) = make_def(&f, "buy", "PFLOW5_GROUP_BUY", "采购管理", false, false).await;
    let (task_id, _) = make_def(&f, "task", "PFLOW5_GROUP_TASK", "任务", false, false).await;
    let (odd_id, _) = make_def(
        &f,
        "odd",
        "PFLOW5_GROUP_ODD",
        "PFLOW5 自定义域",
        false,
        false,
    )
    .await;

    let (groups, total) = load_templates().await;
    assert!(total >= 3, "模板库至少包含本用例的 3 条（实际 {total}）");

    // 每条都在自己 category 的组里
    let buy_group = groups
        .iter()
        .find(|group| group.category == "采购管理")
        .expect("必须有「采购管理」分组");
    assert!(
        buy_group
            .items
            .iter()
            .any(|item| item.id == buy_id.to_string()),
        "采购流程必须落在「采购管理」组"
    );
    let task_group = groups
        .iter()
        .find(|group| group.category == "任务")
        .expect("必须有「任务」分组");
    assert!(
        task_group
            .items
            .iter()
            .any(|item| item.id == task_id.to_string()),
        "任务流程必须落在「任务」组"
    );
    let odd_group = groups
        .iter()
        .find(|group| group.category == "PFLOW5 自定义域")
        .expect("未知分类也要成组（不能丢条目）");
    assert!(
        odd_group
            .items
            .iter()
            .any(|item| item.id == odd_id.to_string())
    );

    // 组顺序：采购管理 → 仓库管理 → 财务管理 → 任务 → 其它
    let pos = |name: &str| groups.iter().position(|group| group.category == name);
    let buy_pos = pos("采购管理").expect("采购管理");
    let task_pos = pos("任务").expect("任务");
    let odd_pos = pos("PFLOW5 自定义域").expect("自定义域");
    assert!(buy_pos < task_pos, "「采购管理」必须排在「任务」之前");
    assert!(task_pos < odd_pos, "未知分类必须排在已知业务域之后");

    // 分组条目总数 = total（分组合计不漏条目）
    let sum: u64 = groups.iter().map(|g| g.items.len() as u64).sum();
    assert_eq!(sum, total, "分组条目合计必须等于 total");

    f.cleanup().await;
}

// ==================== ② is_system ====================

#[tokio::test]
async fn templates_marks_system_defs() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("sysflag").await;
    f.reset().await;

    let (system_id, _) = make_def(&f, "sys", "PFLOW5_SYS_A", "采购管理", false, false).await;
    let (custom_id, _) = make_def(&f, "custom", "PFLOW5_SYS_B", "采购管理", false, false).await;
    db().execute_with_params(
        "UPDATE biz_flow_def SET is_system = TRUE WHERE id = ?::uuid",
        vec![Value::from(&system_id)],
    )
    .await
    .expect("打系统标记");

    let (groups, _) = load_templates().await;
    let system_item = find_item(&groups, system_id).expect("系统流程必须在模板库里");
    let custom_item = find_item(&groups, custom_id).expect("客户流程必须在模板库里");
    assert!(
        system_item.is_system,
        "预置流程必须 is_system = true（前端标「标准」）"
    );
    assert!(!custom_item.is_system, "客户自建流程必须 is_system = false");

    f.cleanup().await;
}

// ==================== ③ usage_count ====================

#[tokio::test]
async fn templates_usage_count_from_instances() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("usage").await;
    f.reset().await;

    let (def_id, _) = make_def(&f, "used", "PFLOW5_USAGE", "采购管理", false, true).await;
    let (idle_id, _) = make_def(&f, "idle", "PFLOW5_USAGE_IDLE", "采购管理", false, true).await;

    start_instance(&f, def_id, "PFLOW5 用一次").await;
    start_instance(&f, def_id, "PFLOW5 用两次").await;

    let (groups, _) = load_templates().await;
    let used = find_item(&groups, def_id).expect("用过的流程在模板库");
    let idle = find_item(&groups, idle_id).expect("没用的流程也在模板库");
    assert_eq!(
        used.usage_count, 2,
        "usage_count 必须等于 biz_flow_instance 的真实条数"
    );
    assert!(used.last_used_at.is_some(), "用过就必须有 last_used_at");
    assert_eq!(idle.usage_count, 0, "没发起过 → 0");
    assert!(idle.last_used_at.is_none(), "没发起过 → last_used_at 为空");

    f.cleanup().await;
}

// ==================== ① 补充：摘要取条件标签 ====================

#[tokio::test]
async fn templates_summary_from_condition_labels() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("summary").await;
    f.reset().await;

    let (def_id, version_id) =
        make_def(&f, "sum", "PFLOW5_SUMMARY", "财务管理", false, false).await;
    // 真分支：分流到审批节点、带阈值标签
    db().execute_with_params(
        "UPDATE biz_flow_edge SET condition_label = '金额 < 10000' \
          WHERE version_id = ?::uuid",
        vec![Value::from(&version_id)],
    )
    .await
    .expect("给真分支打条件标签");
    // 网关中转边：目标节点是 CONDITION（网关 → 网关），标签是「其余区间」的残句，必须被摘要过滤
    db().execute_with_params(
        "INSERT INTO biz_flow_node (id, version_id, node_key, node_name, node_type, config) \
         VALUES (gen_random_uuid(), ?::uuid, 'n_gateway_probe', '金额分级②（< 10 万）', 'CONDITION', '{}'::jsonb)",
        vec![Value::from(&version_id)],
    )
    .await
    .expect("补一个网关节点");
    db().execute_with_params(
        "INSERT INTO biz_flow_edge (id, version_id, edge_key, from_node_id, to_node_id, \
                                   condition_label, priority, is_default) \
         SELECT gen_random_uuid(), e.version_id, 'e_gateway_probe', e.from_node_id, n.id, \
                '金额 ≥ 10000', 10, TRUE \
           FROM biz_flow_edge e, biz_flow_node n \
          WHERE e.version_id = ?::uuid AND n.version_id = ?::uuid AND n.node_key = 'n_gateway_probe' \
          LIMIT 1",
        vec![Value::from(&version_id), Value::from(&version_id)],
    )
    .await
    .expect("补一条网关中转边");

    let (groups, _) = load_templates().await;
    let item = find_item(&groups, def_id).expect("流程在模板库里");
    assert!(
        item.summary.contains("金额 < 10000"),
        "摘要必须取条件连线的分流标签（实际：{}）",
        item.summary
    );
    assert!(
        !item.summary.contains("金额 ≥ 10000"),
        "网关之间的中转边（目标节点是 CONDITION）不能进摘要，否则「< 1 万」会和「≥ 1 万」并列（实际：{}）",
        item.summary
    );
    assert!(
        !item.summary.contains("第二句不该出现"),
        "摘要只取首句 / 标签，不该把整段说明倒出来"
    );

    f.cleanup().await;
}

// ==================== ④ 另存为 ====================

#[tokio::test]
async fn duplicate_creates_draft_copy_with_graph() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("dup").await;
    f.reset().await;

    let (source_id, source_version_id) =
        make_def(&f, "src", "PFLOW5_DUP", "采购管理", false, true).await;
    let source_nodes = count_rows(
        "SELECT count(*) AS total FROM biz_flow_node WHERE version_id = ?::uuid",
        source_version_id,
    )
    .await;
    let source_edges = count_rows(
        "SELECT count(*) AS total FROM biz_flow_edge WHERE version_id = ?::uuid",
        source_version_id,
    )
    .await;
    assert!(source_nodes > 0, "源模板必须有节点");

    let new_code = format!("{}copy_{}", f.prefix(), tail());
    let res = flow_handler::duplicate(
        f.ext(),
        Path(source_id.to_string()),
        Json(flow_dto::DuplicateReq {
            new_flow_code: new_code.clone(),
            new_flow_name: "PFLOW5 我们的采购审批".to_string(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("另存为失败：{} {}", error.code, error.msg))
    .data
    .expect("另存为必须返回回执");

    assert_eq!(res.status, "DRAFT", "副本恒 DRAFT（用户改完自己发布）");
    assert!(!res.is_system, "副本是客户的，不能标「标准」");
    assert_eq!(res.version_no, 1);
    assert_eq!(res.node_count, source_nodes, "节点必须完整复制");
    assert_eq!(res.edge_count, source_edges, "连线必须完整复制");

    let new_def_id = Uuid::parse_str(&res.id).expect("新 def id");
    let new_version_id = Uuid::parse_str(&res.version_id).expect("新版本 id");

    // 库里的事实：状态 / 标记 / 不抢默认 / 没有发布版本
    let rows = db()
        .find_with_params(
            "SELECT status, is_system, is_default, current_version_id, biz_type, category \
               FROM biz_flow_def WHERE id = ?::uuid",
            vec![Value::from(&new_def_id)],
        )
        .await
        .expect("查副本定义");
    let row = rows.first().expect("副本定义必须存在");
    assert_eq!(row.get_typed::<String>("status").unwrap(), "DRAFT");
    assert!(
        !row.get_typed::<bool>("is_system").unwrap(),
        "副本 is_system 必须 false"
    );
    assert!(
        !row.get_typed::<bool>("is_default").unwrap(),
        "副本不能抢占默认流程（否则等于偷偷改客户在跑的规则）"
    );
    assert!(
        row.get_typed::<Option<Uuid>>("current_version_id")
            .unwrap()
            .is_none(),
        "副本还没有发布版本"
    );
    assert_eq!(
        row.get_typed::<String>("biz_type").unwrap(),
        "PFLOW5_DUP",
        "业务类型随源模板"
    );
    assert_eq!(
        row.get_typed::<Option<String>>("category")
            .unwrap()
            .as_deref(),
        Some("采购管理")
    );

    // 图行数一致（节点 / 边 / 审批人）
    let copied_nodes = count_rows(
        "SELECT count(*) AS total FROM biz_flow_node WHERE version_id = ?::uuid",
        new_version_id,
    )
    .await;
    let copied_edges = count_rows(
        "SELECT count(*) AS total FROM biz_flow_edge WHERE version_id = ?::uuid",
        new_version_id,
    )
    .await;
    assert_eq!(copied_nodes, source_nodes, "副本节点行数必须与源一致");
    assert_eq!(copied_edges, source_edges, "副本连线行数必须与源一致");

    // 源模板一行没动
    let source_rows = db()
        .find_with_params(
            "SELECT status, is_system FROM biz_flow_def WHERE id = ?::uuid",
            vec![Value::from(&source_id)],
        )
        .await
        .expect("查源定义");
    let source = source_rows.first().expect("源定义还在");
    assert_eq!(source.get_typed::<String>("status").unwrap(), "PUBLISHED");
    assert!(!source.get_typed::<bool>("is_system").unwrap());

    f.cleanup().await;
}

// ==================== 红线 4：不复制实例 ====================

#[tokio::test]
async fn duplicate_does_not_copy_instances() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("dupinst").await;
    f.reset().await;

    let (source_id, _) = make_def(&f, "src", "PFLOW5_DUP_INST", "财务管理", false, true).await;
    start_instance(&f, source_id, "PFLOW5 原模板的单子").await;
    let source_instances = count_rows(
        "SELECT count(*) AS total FROM biz_flow_instance WHERE def_id = ?::uuid",
        source_id,
    )
    .await;
    assert_eq!(source_instances, 1);

    let new_code = format!("{}c_{}", f.prefix(), tail());
    let res = flow_handler::duplicate(
        f.ext(),
        Path(source_id.to_string()),
        Json(flow_dto::DuplicateReq {
            new_flow_code: new_code,
            new_flow_name: "PFLOW5 副本".to_string(),
        }),
    )
    .await
    .expect("另存为必须成功")
    .data
    .expect("回执");
    let new_def_id = Uuid::parse_str(&res.id).expect("新 def id");

    let copied_instances = count_rows(
        "SELECT count(*) AS total FROM biz_flow_instance WHERE def_id = ?::uuid",
        new_def_id,
    )
    .await;
    assert_eq!(copied_instances, 0, "另存为只复制定义，绝不复制实例数据");
    let after = count_rows(
        "SELECT count(*) AS total FROM biz_flow_instance WHERE def_id = ?::uuid",
        source_id,
    )
    .await;
    assert_eq!(after, 1, "原模板的历史实例必须原样留着");

    f.cleanup().await;
}

// ==================== ⑤ 编码冲突 ====================

#[tokio::test]
async fn duplicate_conflict_on_existing_code() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("dup409").await;
    f.reset().await;

    let (source_id, _) = make_def(&f, "src", "PFLOW5_DUP409", "采购管理", false, true).await;
    let new_code = format!("{}taken_{}", f.prefix(), tail());
    flow_handler::duplicate(
        f.ext(),
        Path(source_id.to_string()),
        Json(flow_dto::DuplicateReq {
            new_flow_code: new_code.clone(),
            new_flow_name: "PFLOW5 第一次".to_string(),
        }),
    )
    .await
    .expect("第一次另存为要成功");

    let error = flow_handler::duplicate(
        f.ext(),
        Path(source_id.to_string()),
        Json(flow_dto::DuplicateReq {
            new_flow_code: new_code.clone(),
            new_flow_name: "PFLOW5 第二次".to_string(),
        }),
    )
    .await
    .expect_err("重复编码必须 409");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);

    // 不能留下半成品：该编码只应有 1 条未删定义
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_def WHERE flow_code = ? AND deleted_at IS NULL",
            vec![Value::Text(new_code.clone())],
        )
        .await
        .expect("查编码占用");
    let total: i64 = rows.first().unwrap().get_typed("total").unwrap();
    assert_eq!(total, 1, "冲突失败不能留下半成品定义");

    f.cleanup().await;
}

// ==================== ⑥ 停用默认流程 ====================

#[tokio::test]
async fn offline_default_leaves_biz_type_without_default() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("offdefault").await;
    f.reset().await;

    let biz_type = format!("PFLOW5_OFF_{}", f.tag.to_ascii_uppercase());
    let (def_id, _) = make_def(&f, "def", &biz_type, "采购管理", true, true).await;

    let before = flow_model::default_def_of_biz_type(&biz_type)
        .await
        .expect("解析默认流程");
    assert_eq!(
        before.map(|row| row.id),
        Some(def_id),
        "停用前该 biz_type 的默认流程就是它"
    );

    flow_handler::offline(f.ext(), Path(def_id.to_string()))
        .await
        .expect("停用必须成功");

    let after = flow_model::default_def_of_biz_type(&biz_type)
        .await
        .expect("解析默认流程");
    assert!(
        after.is_none(),
        "停用默认流程后该 biz_type 必须解析不到默认流程（后续提交直接生效、不走审批）"
    );

    f.cleanup().await;
}

// ==================== ⑦ 停用非默认 ====================
#[tokio::test]
async fn offline_non_default_keeps_other_default() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("offother").await;
    f.reset().await;

    let biz_type = format!("PFLOW5_OFF2_{}", f.tag.to_ascii_uppercase());
    let (default_id, _) = make_def(&f, "keep", &biz_type, "采购管理", true, true).await;
    let (other_id, _) = make_def(&f, "other", &biz_type, "采购管理", false, true).await;

    flow_handler::offline(f.ext(), Path(other_id.to_string()))
        .await
        .expect("停用非默认流程");

    let after = flow_model::default_def_of_biz_type(&biz_type)
        .await
        .expect("解析默认流程")
        .expect("默认流程必须还在");
    assert_eq!(
        after.id, default_id,
        "停用非默认流程不能影响同业务域的默认流程"
    );

    f.cleanup().await;
}

// ==================== ⑨ 恢复启用（停用 → 启用 → 恢复走审批） ====================

#[tokio::test]
async fn enable_restores_default_after_offline() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("redefault").await;
    f.reset().await;

    let biz_type = format!("PFLOW5_REENABLE_{}", f.tag.to_ascii_uppercase());
    let (def_id, version_id) = make_def(&f, "re", &biz_type, "采购管理", true, true).await;

    // 停用 → 该 biz_type 解析不到默认流程（提交直接生效、不走审批）
    flow_handler::offline(f.ext(), Path(def_id.to_string()))
        .await
        .expect("停用必须成功");
    assert!(
        flow_model::default_def_of_biz_type(&biz_type)
            .await
            .expect("解析默认流程")
            .is_none(),
        "停用后不该再有默认流程"
    );

    // 启用 → 默认流程回来（复用停用前那个发布版本，不产生新版本）
    let enabled = flow_handler::enable(f.ext(), Path(def_id.to_string()))
        .await
        .unwrap_or_else(|error| panic!("恢复启用失败：{} {}", error.code, error.msg))
        .data
        .expect("启用回执");
    assert_eq!(enabled.status, "PUBLISHED", "启用后状态必须是 PUBLISHED");

    let after = flow_model::default_def_of_biz_type(&biz_type)
        .await
        .expect("解析默认流程")
        .expect("启用后必须重新解析到默认流程（恢复走审批）");
    assert_eq!(after.id, def_id);
    assert_eq!(
        after.current_version_id,
        Some(version_id),
        "恢复启用不产生新版本，仍指向停用前那个发布版本"
    );

    // 版本数没变（启用只拨状态位）
    let versions = count_rows(
        "SELECT count(*) AS total FROM biz_flow_def_version WHERE def_id = ?::uuid",
        def_id,
    )
    .await;
    assert_eq!(versions, 1, "恢复启用不新增版本");

    // 启用过的再启用 → 409（不是 DISABLED）
    let error = flow_handler::enable(f.ext(), Path(def_id.to_string()))
        .await
        .expect_err("已启用的流程再启用必须 409");
    assert_eq!(error.status, 409);

    f.cleanup().await;
}

// ==================== ⑩ 草稿不能靠「启用」绕过发布校验 ====================

#[tokio::test]
async fn enable_refuses_draft_definitions() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("enabledraft").await;
    f.reset().await;

    // 只建不发布 → status = DRAFT（没有发布版本）
    let (def_id, _) = make_def(&f, "draft", "PFLOW5_ENABLE_DRAFT", "采购管理", false, false).await;

    let error = flow_handler::enable(f.ext(), Path(def_id.to_string()))
        .await
        .expect_err("草稿不能走「启用」（否则绕过 12 条发布校验）");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::FLOW_VERSION_NOT_EDITABLE);

    // 状态没被改动
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_flow_def WHERE id = ?::uuid",
            vec![Value::from(&def_id)],
        )
        .await
        .expect("查状态");
    assert_eq!(
        rows.first().unwrap().get_typed::<String>("status").unwrap(),
        "DRAFT"
    );

    f.cleanup().await;
}

// ==================== ⑧ 只读流程图接口 ====================

#[tokio::test]
async fn read_only_graph_endpoint_returns_nodes_edges() {
    let Some(()) = ready().await else { return };
    let f = Fixture::new("graph").await;
    f.reset().await;

    let (source_id, source_version_id) =
        make_def(&f, "g", "PFLOW5_GRAPH", "仓库管理", false, true).await;
    let source_nodes = count_rows(
        "SELECT count(*) AS total FROM biz_flow_node WHERE version_id = ?::uuid",
        source_version_id,
    )
    .await;
    let source_edges = count_rows(
        "SELECT count(*) AS total FROM biz_flow_edge WHERE version_id = ?::uuid",
        source_version_id,
    )
    .await;

    // 副本（DRAFT）也要能看图 —— 模板库里点「查看流程图」走的就是这条路径
    let new_code = format!("{}view_{}", f.prefix(), tail());
    let dup = flow_handler::duplicate(
        f.ext(),
        Path(source_id.to_string()),
        Json(flow_dto::DuplicateReq {
            new_flow_code: new_code,
            new_flow_name: "PFLOW5 看图副本".to_string(),
        }),
    )
    .await
    .expect("另存为")
    .data
    .expect("回执");

    let view = flow_handler::version_detail(Path(dup.version_id.clone()))
        .await
        .expect("只读流程图接口必须成功")
        .data
        .expect("版本详情");
    assert_eq!(
        view.nodes.len() as i64,
        source_nodes,
        "节点必须能完整读回来"
    );
    assert_eq!(
        view.edges.len() as i64,
        source_edges,
        "连线必须能完整读回来"
    );
    assert!(view.editable, "DRAFT 版本可编辑");
    assert!(!view.has_snapshot, "DRAFT 没有发布快照");
    assert!(
        view.nodes.iter().any(|node| node.node_type == "START"),
        "图里必须有 START 节点"
    );

    // 发布版（只读）同样能读
    let published = flow_handler::version_detail(Path(source_version_id.to_string()))
        .await
        .expect("发布版本详情")
        .data
        .expect("发布版本详情");
    assert!(!published.editable, "PUBLISHED 版本只读");
    assert!(published.has_snapshot, "PUBLISHED 必须有快照");
    assert_eq!(published.nodes.len() as i64, source_nodes);

    f.cleanup().await;
}
