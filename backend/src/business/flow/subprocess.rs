//! 子流程（`SUBPROCESS`）节点：配置解析、发布校验、子实例发起、父子结算回调（P3-08 · ARCH-5 §4.14）。
//!
//! # 一条句子说清设计
//!
//! 父流程的 `SUBPROCESS` 节点保持 `ACTIVE`，**在同一个事务里**用
//! [`engine::start_in_tx`] 建一张真正的子审批实例（跑子流程自己的图）；子实例结算时
//! 回调父节点 —— 通过则父节点 `APPROVED` 继续往下走，驳回且 [`FailAction::Abort`]
//! 则整个父实例 `REJECTED`。
//!
//! # 为什么是「真子实例」而不是「把子流程的图展开进来」
//!
//! 展开（inline expand）会让子流程的节点 key 撞父流程的 key，只能加前缀；前缀一加，
//! 子流程改版就得重新展开所有父流程，且子流程自己的审批记录、超时、统计全混进父实例，
//! 谁也说不清「这张单到底卡在哪」。真子实例的代价只是多两列
//! （`parent_instance_id` / `parent_node_key`），换来的是「子流程是独立单据」这个事实。
//!
//! # 子实例的 `biz_id` 一律为 `NULL`（卡面「决策」）
//!
//! `uk_biz_flow_instance_biz_active` 是
//! `(biz_type, biz_id) WHERE status='PENDING' AND biz_id IS NOT NULL` 的**部分**唯一索引。
//! 子实例如果照抄父实例的 `biz_id`，第二条子实例（或父实例自己）就会撞唯一键，把父流程卡死。
//! 留 `NULL` ⇒ 既不撞父流程、也不与兄弟子实例相撞；父子关系靠
//! `parent_instance_id + parent_node_key`（V0341 的部分索引）精确定位。
//!
//! # 幂等（卡面硬约束）
//!
//! 回调走两把闸：
//! ① 父实例不是 `PENDING` → 直接跳过（重放 / 父单已终结）；
//! ② 父节点行已是终态（`APPROVED` / `REJECTED` / `CANCELLED`）→ 直接跳过。
//! 与 [`super::task_bridge::on_instance_settled`] 同款写法：**判定在事务内，日志不刷屏**。
//!
//! # 版本策略（`config.sub.version_policy`）
//!
//! | 值 | 含义 | 实现 |
//! |---|---|---|
//! | `PIN_CURRENT`（默认） | 钉住本节点第一次发起子实例时解析出的版本 | 同一 `(父实例, 节点)` 已经有子实例 → 复用它绑的 `version_id`（退回后重走同一节点不换版本） |
//! | `FOLLOW_LATEST` | 每次进入都取子流程**当下**的当前发布版本 | 每次重新解析 `biz_flow_def.current_version_id` |
//!
//! 两者的共同点：**子实例永远绑在一个具体 `version_id` 上**（引擎 D2），子流程之后
//! 改版不会改变已经发出去的子单。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value as Json};

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::engine::{self, StartFailure, StartReq};
use crate::business::flow::graph::{FlowGraph, Issue, NodeType, SnapshotNode};
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// 允许的最大嵌套深度（本流程 = 第 1 层，它的子流程 = 第 2 层 …）。
///
/// 3 层的口径来自 ARCH-5 §4.14：`报销 → 预算调整 → 部门预算` 是三层的真实场景，
/// 再深就该拆流程了；真正的理由是**深度是递归风险的乘数**（每层都是一个真实例，
/// 一份脏配置能在一张单上挂出一棵无限深的树）。
pub const MAX_NEST_DEPTH: u32 = 3;

/// `config.sub` 的全部合法键（`additionalProperties: false` 的唯一依据）。
pub const SUB_CONFIG_KEYS: [&str; 5] = [
    "version_policy",
    "fail_action",
    "param_mapping",
    "result_mapping",
    "spawn_task",
];

// ==================== 枚举 ====================

/// 子流程版本策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionPolicy {
    /// 钉住第一次发起时解析出的版本（默认）。
    #[default]
    PinCurrent,
    /// 每次进入都跟随子流程的最新发布版本。
    FollowLatest,
}

impl VersionPolicy {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            VersionPolicy::PinCurrent => "PIN_CURRENT",
            VersionPolicy::FollowLatest => "FOLLOW_LATEST",
        }
    }

    /// 解析（大小写不敏感；空 / 未知 → `None`）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "PIN_CURRENT" => Some(VersionPolicy::PinCurrent),
            "FOLLOW_LATEST" => Some(VersionPolicy::FollowLatest),
            _ => None,
        }
    }
}

/// 子流程没走完时父流程怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailAction {
    /// 终止父流程（默认）：子流程被驳回 ⇒ 父实例 `REJECTED`。
    #[default]
    Abort,
    /// 继续：子流程被驳回只算「这个节点不通过但仍往下走」（父节点记 `APPROVED`）。
    Continue,
}

impl FailAction {
    /// 出参 / 入库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            FailAction::Abort => "ABORT",
            FailAction::Continue => "CONTINUE",
        }
    }

    /// 解析（大小写不敏感；空 / 未知 → `None`）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ABORT" => Some(FailAction::Abort),
            "CONTINUE" => Some(FailAction::Continue),
            _ => None,
        }
    }
}

// ==================== 配置 ====================

/// `config.sub` 解析后的形态。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubConfig {
    /// 版本策略。
    pub version_policy: VersionPolicy,
    /// 子流程没走完时的动作。
    pub fail_action: FailAction,
    /// 父 `form_data` → 子 `form_data`：**键 = 子流程的键**，值 = 父表单里的路径（`a.b.c`）。
    ///
    /// 方向定死的理由：R27 要校验「映射到的键在子流程表单里存在」——只有键是子侧时这句
    /// 才成立；值指向父侧，父侧没有表单定义，本来就无从校验。
    pub param_mapping: BTreeMap<String, String>,
    /// 子 `form_data` → 父 `form_data`：**键 = 父流程的键**，值 = 子实例表单里的路径。
    pub result_mapping: BTreeMap<String, String>,
    /// 是否在父侧生成一条 `biz_task`（让父流程的人看得见「子流程在跑」）。
    pub spawn_task: bool,
}

/// `config.sub` 的解析错误（带业务码，便于 R27 直接落成 `Issue`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubConfigError {
    /// `error_code::*`（65045）。
    pub code: i32,
    /// 面向用户的中文原因。
    pub message: String,
}

impl SubConfigError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 解析 `config.sub`（**纯函数**：发布校验与运行期共用同一份口径）。
///
/// `null` / 缺省 → 全默认（`PIN_CURRENT` + `ABORT` + 不生成任务）。
/// 非对象 → 65045；未知键 → 65045（拼错键名必须当场报错，别等运行期发现「没生效」）。
pub fn parse_config(value: &Json) -> std::result::Result<SubConfig, SubConfigError> {
    if value.is_null() {
        return Ok(SubConfig::default());
    }
    let Some(object) = value.as_object() else {
        return Err(SubConfigError::new(
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            "子流程配置（config.sub）必须是一个对象",
        ));
    };
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !SUB_CONFIG_KEYS.contains(key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(SubConfigError::new(
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            format!(
                "子流程配置里有不认识的键 {}（只允许 {}）",
                unknown.join(" / "),
                SUB_CONFIG_KEYS.join(" / ")
            ),
        ));
    }

    let version_policy = match object.get("version_policy") {
        None | Some(Json::Null) => VersionPolicy::default(),
        Some(Json::String(raw)) if raw.trim().is_empty() => VersionPolicy::default(),
        Some(Json::String(raw)) => VersionPolicy::parse(raw).ok_or_else(|| {
            SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("子流程版本策略 {raw} 非法（只允许 PIN_CURRENT / FOLLOW_LATEST）"),
            )
        })?,
        Some(_) => {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                "子流程版本策略（version_policy）必须是字符串",
            ));
        }
    };

    let fail_action = match object.get("fail_action") {
        None | Some(Json::Null) => FailAction::default(),
        Some(Json::String(raw)) if raw.trim().is_empty() => FailAction::default(),
        Some(Json::String(raw)) => FailAction::parse(raw).ok_or_else(|| {
            SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("子流程失败动作 {raw} 非法（只允许 ABORT / CONTINUE）"),
            )
        })?,
        Some(_) => {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                "子流程失败动作（fail_action）必须是字符串",
            ));
        }
    };

    let param_mapping = parse_mapping(object.get("param_mapping"), "param_mapping")?;
    let result_mapping = parse_mapping(object.get("result_mapping"), "result_mapping")?;

    let spawn_task = match object.get("spawn_task") {
        None | Some(Json::Null) => false,
        Some(Json::Bool(flag)) => *flag,
        Some(_) => {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                "是否生成任务（spawn_task）必须是布尔值",
            ));
        }
    };

    Ok(SubConfig {
        version_policy,
        fail_action,
        param_mapping,
        result_mapping,
        spawn_task,
    })
}

/// 解析 `param_mapping` / `result_mapping`：对象，值是「表单路径」字符串。
fn parse_mapping(value: Option<&Json>, label: &str) -> std::result::Result<BTreeMap<String, String>, SubConfigError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    if value.is_null() {
        return Ok(BTreeMap::new());
    }
    let Some(object) = value.as_object() else {
        return Err(SubConfigError::new(
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            format!("{label} 必须是一个对象（键 → 表单字段路径）"),
        ));
    };
    let mut mapping = BTreeMap::new();
    for (key, raw) in object {
        let key = key.trim();
        if key.is_empty() {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("{label} 里有空的键"),
            ));
        }
        let Some(path) = raw.as_str() else {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("{label}.{key} 的值必须是表单字段路径字符串"),
            ));
        };
        let path = path.trim();
        if path.is_empty() {
            return Err(SubConfigError::new(
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("{label}.{key} 的路径为空"),
            ));
        }
        mapping.insert(key.to_string(), path.to_string());
    }
    Ok(mapping)
}

// ==================== 嵌套深度（纯函数，可单测） ====================

/// 嵌套检查的两种失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestingProblem {
    /// 自己指自己（直接或间接成环），`path` 是环上的流程定义 id。
    Cycle(Vec<Uuid>),
    /// 嵌套过深，`depth` 是实际深度、`path` 是走到最深处的链路。
    TooDeep { depth: u32, path: Vec<Uuid> },
}

/// 从 `root` 出发算最大嵌套深度（`root` 自身 = 0，每进一层 +1）。
///
/// `children` 是「流程定义 → 它的子流程定义列表」（只含已经查到的部分；缺的按「无子流程」处理）。
/// 成环一律 [`NestingProblem::Cycle`]：环的后果不是「慢」而是**无限发起**。
pub fn nesting_depth(root: Uuid, children: &BTreeMap<Uuid, Vec<Uuid>>) -> std::result::Result<u32, NestingProblem> {
    fn walk(
        node: Uuid,
        depth: u32,
        children: &BTreeMap<Uuid, Vec<Uuid>>,
        path: &mut Vec<Uuid>,
        best: &mut (u32, Vec<Uuid>),
    ) -> std::result::Result<(), NestingProblem> {
        if path.contains(&node) {
            let mut cycle = path.clone();
            cycle.push(node);
            return Err(NestingProblem::Cycle(cycle));
        }
        path.push(node);
        if depth > best.0 {
            *best = (depth, path.clone());
        }
        if let Some(kids) = children.get(&node) {
            for kid in kids {
                walk(*kid, depth + 1, children, path, best)?;
            }
        }
        path.pop();
        Ok(())
    }

    let mut best = (0u32, vec![root]);
    let mut path = Vec::new();
    walk(root, 0, children, &mut path, &mut best)?;
    if best.0 > MAX_NEST_DEPTH {
        return Err(NestingProblem::TooDeep {
            depth: best.0,
            path: best.1,
        });
    }
    Ok(best.0)
}

// ==================== 发布校验（要查库的那一半 R27） ====================

/// R27 里**必须查库**的部分（P3-08 / ARCH-5 §4.14）。
///
/// | 判据 | 错误码 |
/// |---|---|
/// | 子流程定义不存在 / 已软删 / 不是 `PUBLISHED`（没有当前版本） | 65045 |
/// | `subprocess_def_id` 指向自己、间接成环、嵌套深度 > 3 | 65046 |
///
/// `param_mapping` 的「键在子流程表单里存在」这一条**跳过**：本仓**没有表单定义（schema）表**
/// —— `form_data` 是各业务单据自带的 JSONB，没有可枚举字段名的注册表
/// （`grep -rn "form_schema" backend/src` 零命中，`biz_flow_*` 也没有表单字段表）。
/// 硬造一份字段表等于给二开加一个必须同步维护的新概念，风险大于收益；
/// 这里只校验映射的**形状**（[`parse_config`]），运行期取不到的路径按「缺值不填」处理
/// （见 [`map_form_data`]）。等真有表单定义注册表时，把这段注释换成一次查询即可。
pub async fn validate_publish(graph: &FlowGraph, def_id: Uuid) -> DbResult<Vec<Issue>> {
    let mut issues = Vec::new();
    let mut checked_defs: BTreeSet<Uuid> = BTreeSet::new();
    let mut children: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();

    for node in &graph.nodes {
        if node.node_type != NodeType::Subprocess {
            continue;
        }
        let Some(target) = node.subprocess_def_id else {
            continue; // 静态 R27 已报「没选子流程」
        };

        // ① 直接自递归：不用查库就能判
        if target == def_id {
            issues.push(
                Issue::new(
                    "R27",
                    error_code::FLOW_SUBPROCESS_RECURSION,
                    format!(
                        "子流程节点 {}（{}）指向了本流程自己（{target}）—— 自递归会在发起时无限建子实例",
                        node.key, node.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            );
            continue;
        }

        // ② 定义必须存在、未软删、已发布
        let Some(row) = load_def(db(), target).await? else {
            issues.push(
                Issue::new(
                    "R27",
                    error_code::FLOW_SUBPROCESS_DEF_INVALID,
                    format!(
                        "子流程节点 {}（{}）指向的流程定义 {target} 不存在或已删除",
                        node.key, node.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            );
            continue;
        };
        if !row.status.eq_ignore_ascii_case("PUBLISHED") || row.current_version_id.is_none() {
            issues.push(
                Issue::new(
                    "R27",
                    error_code::FLOW_SUBPROCESS_DEF_INVALID,
                    format!(
                        "子流程节点 {}（{}）指向的流程「{}」当前状态是 {}（必须是已发布且有当前版本）",
                        node.key, node.name, row.flow_code, row.status
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            );
            continue;
        }

        if checked_defs.insert(target) {
            children.extend(load_children_of(target).await?);
        }
    }

    // ③ 嵌套深度 / 间接成环：从根算到底（每个根只报一条，避免刷屏）
    let root_has_subprocess = graph
        .nodes
        .iter()
        .any(|node| node.node_type == NodeType::Subprocess && node.subprocess_def_id.is_some());
    if root_has_subprocess {
        // ★ 根自己的子流程引用必须取**正在发布的这张图**，不能用 `load_children_of(def_id)`：
        //   此刻根的 `biz_flow_def.current_version_id` 还没指向这张草稿（发布事务在最后一步才更新），
        //   按当前版本 JOIN 会永远查空 ⇒ 深度恒等于 0（P3-08 第一版就栽在这，深度 4 层发布成功）。
        let root_kids: Vec<Uuid> = graph
            .nodes
            .iter()
            .filter(|node| node.node_type == NodeType::Subprocess)
            .filter_map(|node| node.subprocess_def_id)
            .collect();
        children.insert(def_id, root_kids);
        // 把子树补全到 MAX_NEST_DEPTH + 1 层（够判断「超限」，不无限展开）
        let mut pending: VecDeque<(Uuid, u32)> = VecDeque::new();
        for kids in children.values() {
            for kid in kids {
                pending.push_back((*kid, 1));
            }
        }
        while let Some((current, depth)) = pending.pop_front() {
            if depth > MAX_NEST_DEPTH {
                continue;
            }
            if children.contains_key(&current) {
                continue;
            }
            let loaded = load_children_of(current).await?;
            for kids in loaded.values() {
                for kid in kids {
                    pending.push_back((*kid, depth + 1));
                }
            }
            children.extend(loaded);
        }
        let subprocess_nodes: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.node_type == NodeType::Subprocess)
            .map(|node| node.key.clone())
            .collect();
        let verdict = nesting_depth(def_id, &children);
        tracing::info!(
            def_id = %def_id,
            subprocess_nodes = subprocess_nodes.len(),
            defs_loaded = children.len(),
            depth = ?verdict.as_ref().map(|depth| *depth).map_err(|problem| format!("{problem:?}")),
            "R27 嵌套深度检查（发布期查库部分）"
        );
        match verdict {
            Ok(_) => {}
            Err(NestingProblem::Cycle(path)) => {
                issues.push(
                    Issue::new(
                        "R27",
                        error_code::FLOW_SUBPROCESS_RECURSION,
                        format!("子流程引用成环（{}）—— 环上的流程会在发起时无限嵌套", join_ids(&path)),
                    )
                    .with_nodes(subprocess_nodes),
                );
            }
            Err(NestingProblem::TooDeep { depth, path }) => {
                issues.push(
                    Issue::new(
                        "R27",
                        error_code::FLOW_SUBPROCESS_RECURSION,
                        format!(
                            "子流程嵌套深度 {depth} 层超过上限 {MAX_NEST_DEPTH}（链路 {}）",
                            join_ids(&path)
                        ),
                    )
                    .with_nodes(subprocess_nodes),
                );
            }
        }
    }

    Ok(issues)
}

/// 把流程定义 id 链拼成人读文案。
fn join_ids(ids: &[Uuid]) -> String {
    ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(" → ")
}

/// 流程定义的一行（发布校验 / 发起子实例共用）。
#[derive(Debug, Clone)]
struct DefRow {
    flow_code: String,
    flow_name: String,
    biz_type: String,
    status: String,
    current_version_id: Option<Uuid>,
    /// 本行「要用的版本号」：`load_def` 填 `current_version_no`，`load_version_def` 填该版本的 `version_no`。
    version_no: i32,
}

async fn load_def(ex: &impl SqlExecutor, def_id: Uuid) -> DbResult<Option<DefRow>> {
    let rows = ex
        .query_sql(
            "SELECT flow_code, flow_name, biz_type, status, current_version_id, current_version_no \
               FROM biz_flow_def WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(DefRow {
        flow_code: row.get_typed("flow_code")?,
        flow_name: row.get_typed("flow_name")?,
        biz_type: row.get_typed("biz_type")?,
        status: row.get_typed("status")?,
        current_version_id: row.get_typed("current_version_id")?,
        version_no: row.get_typed("current_version_no")?,
    }))
}

/// 一个流程定义（按它的**当前发布版本**）引用了哪些子流程定义。
async fn load_children_of(def_id: Uuid) -> DbResult<BTreeMap<Uuid, Vec<Uuid>>> {
    let rows = db()
        .query_sql(
            "SELECT n.subprocess_def_id AS sub_def \
               FROM biz_flow_node n \
               JOIN biz_flow_def d ON d.current_version_id = n.version_id \
              WHERE d.id = ?::uuid AND n.node_type = 'SUBPROCESS' \
                AND n.subprocess_def_id IS NOT NULL"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    let mut kids = Vec::new();
    for row in &rows {
        if let Some(id) = row.get_typed::<Option<Uuid>>("sub_def")? {
            kids.push(id);
        }
    }
    let mut map = BTreeMap::new();
    map.insert(def_id, kids);
    Ok(map)
}

// ==================== 发起子实例（事务内） ====================

/// 子实例发起结果。
#[derive(Debug, Clone)]
pub struct Spawned {
    /// 子实例 id。
    pub instance_id: Uuid,
    /// 子实例状态（`PENDING` = 在跑，其他 = 事务内已经跑完）。
    pub status: String,
    /// 子流程定义。
    pub def_id: Uuid,
    /// 子实例绑的版本。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
}

/// 在**父实例的事务里**建一张子实例（复用 [`engine::start_in_tx`]，不另写一套发起逻辑）。
///
/// # 顺序（不能换）
///
/// ① 运行时再查一次嵌套深度（发布校验只是「那时」的结论，配置可以被直接改库绕过）；
/// ② 解析子流程定义与版本（`PIN_CURRENT` / `FOLLOW_LATEST`）；
/// ③ 映射 `form_data`；
/// ④ `start_in_tx` 发起（**业务拒绝** ⇒ 整笔回滚，父节点不会停在 ACTIVE）；
/// ⑤ 回写 `parent_instance_id` / `parent_node_key`（子实例只有建完之后才有 id）。
///
/// # 异常情况
///
/// - 子流程定义非法 / 未发布 → 65045；
/// - 自递归 / 深度超限 → 65046；
/// - 子实例发起被拒（没有可发版本 / 没有 START / 派不出审批人）→ 65047；
/// - 数据库异常 → 沿用 `start_in_tx` 的 [`DbError`]。
pub async fn spawn_child(
    ex: &Transaction,
    parent: &flow_model::InstanceRow,
    spec: &SnapshotNode,
    round: i32,
    cfg: &SubConfig,
) -> std::result::Result<Spawned, spawn_error::Error> {
    // ① 运行时深度闸（含自递归）
    let depth = runtime_depth(ex, parent).await?;
    if depth > MAX_NEST_DEPTH {
        return Err(spawn_error::Error::api(ApiError::new(
            422,
            error_code::FLOW_SUBPROCESS_RECURSION,
            format!(
                "子流程嵌套已经 {depth} 层，超过上限 {MAX_NEST_DEPTH}；不允许再往下发起（节点 {}）",
                spec.key
            ),
        )));
    }

    // ② 定义 + 版本
    //
    // 指针只存在**物理列** `biz_flow_node.subprocess_def_id` 上（不在快照 JSON 里）：外键只能
    // 建在列上，而 PUBLISHED 版本的节点行本身就是冻结的（`replace_graph` 只动 DRAFT），
    // 所以「发布后列不变」这条与「快照冻结」等价 —— 少改快照格式，存量 4 个 PUBLISHED 版本的
    // `graph_hash` 一个字节都不动。
    let target_def = node_subprocess_def(ex, parent.version_id, &spec.key)
        .await?
        .ok_or_else(|| {
            spawn_error::Error::api(ApiError::new(
                422,
                error_code::FLOW_SUBPROCESS_DEF_INVALID,
                format!("子流程节点 {} 没有指向合法的子流程定义", spec.key),
            ))
        })?;
    if target_def == parent.def_id {
        return Err(spawn_error::Error::api(ApiError::new(
            422,
            error_code::FLOW_SUBPROCESS_RECURSION,
            format!("子流程节点 {} 指向了本流程自己，自递归被拒绝", spec.key),
        )));
    }
    let (def, version_id) = resolve_version(ex, parent, spec, target_def, cfg).await?;

    // ③ 表单映射
    let form_data = map_form_data(&parent.form_data, &cfg.param_mapping);

    // ④ 发起
    let instance_id = Uuid::now_v7();
    let title = child_title(&def.flow_name, &parent.title);
    let req = StartReq {
        instance_id,
        def_id: target_def,
        version_id,
        version_no: def.version_no,
        flow_code: def.flow_code.clone(),
        flow_name: def.flow_name.clone(),
        biz_type: def.biz_type.clone(),
        // ★ 卡面决策：子实例不继承父单据的 biz_id —— 否则撞 uk_biz_flow_instance_biz_active
        biz_id: None,
        biz_no: None,
        title,
        initiator_id: parent.initiator_id,
        initiator_name: parent.initiator_name.clone(),
        initiator_dept_id: parent.initiator_dept_id,
        form_data,
    };
    // ★ 递归边的唯一一处 `Box::pin`：子流程发起会**递归**回到 `engine::start`（子流程里还可能
    //   有子流程），不装箱的话 `start` 的 future 类型会无限嵌套（编译期 E0733）。
    //   装箱只发生在这里 —— 一次子实例发起多一次堆分配，换掉一个不可能收敛的类型。
    let out = match Box::pin(engine::start_in_tx(ex, &req)).await {
        Ok(out) => out,
        Err(StartFailure::Rejected(error)) => {
            return Err(spawn_error::Error::api(ApiError::new(
                422,
                error_code::FLOW_SUBPROCESS_START_FAILED,
                format!("子流程「{}」在节点 {} 发起失败：{}", def.flow_code, spec.key, error.msg),
            )));
        }
        Err(StartFailure::Db(error)) => return Err(spawn_error::Error::db(error)),
    };

    // ⑤ 回写父子关系
    ex.execute_with_params(
        "UPDATE biz_flow_instance \
            SET parent_instance_id = ?::uuid, parent_node_key = ?, updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::from(&parent.id),
            Value::Text(spec.key.clone()),
            Value::from(&instance_id),
        ],
    )
    .await?;

    tracing::info!(
        parent_instance_id = %parent.id,
        child_instance_id = %instance_id,
        node_key = %spec.key,
        round,
        child_flow = %def.flow_code,
        child_version_no = def.version_no,
        child_status = %out.instance_status,
        version_policy = cfg.version_policy.as_str(),
        "子流程实例已在父事务内发起"
    );

    Ok(Spawned {
        instance_id,
        status: out.instance_status,
        def_id: target_def,
        version_id,
        version_no: def.version_no,
    })
}

/// 发起子实例时的错误出口（避免 `ApiError` 与 `DbError` 在签名里打架）。
pub mod spawn_error {
    use super::*;

    /// 两种失败：业务拒绝（变成 `Step::Fail`）/ 数据库异常（整单回滚）。
    #[derive(Debug)]
    pub enum Error {
        /// 业务拒绝。
        Api(ApiError),
        /// 数据库异常。
        Db(DbError),
    }

    impl Error {
        /// 业务拒绝。
        pub fn api(error: ApiError) -> Self {
            Error::Api(error)
        }

        /// 数据库异常。
        pub fn db(error: DbError) -> Self {
            Error::Db(error)
        }
    }

    impl From<DbError> for Error {
        fn from(error: DbError) -> Self {
            Error::Db(error)
        }
    }
}

/// 按 (版本, 节点 key) 读子流程定义指针（`biz_flow_node.subprocess_def_id`）。
///
/// 读列而不是读快照：见 [`spawn_child`] 的 ② 段注释（PUBLISHED 版本的节点行是冻结的）。
async fn node_subprocess_def(ex: &Transaction, version_id: Uuid, node_key: &str) -> DbResult<Option<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT subprocess_def_id FROM biz_flow_node \
              WHERE version_id = ?::uuid AND node_key = ?",
            vec![Value::from(&version_id), Value::Text(node_key.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("subprocess_def_id"),
    }
}

/// 父实例链上的嵌套层数（顶层实例 = 0；它的子实例 = 1 …）。
async fn runtime_depth(ex: &Transaction, parent: &flow_model::InstanceRow) -> DbResult<u32> {
    // 一条递归 CTE 拿到整条链（`parent_instance_id` 自带部分索引）
    let rows = ex
        .find_with_params(
            "WITH RECURSIVE chain AS ( \
                 SELECT id, parent_instance_id, 0 AS depth FROM biz_flow_instance WHERE id = ?::uuid \
                 UNION ALL \
                 SELECT i.id, i.parent_instance_id, c.depth + 1 \
                   FROM biz_flow_instance i JOIN chain c ON i.id = c.parent_instance_id \
                  WHERE c.depth < 32 \
             ) SELECT COALESCE(max(depth), 0) AS depth FROM chain",
            vec![Value::from(&parent.id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => {
            let depth: i32 = row.get_typed("depth")?;
            Ok(depth as u32)
        }
    }
}

/// 解析子流程定义与要绑的版本号。
async fn resolve_version(
    ex: &Transaction,
    parent: &flow_model::InstanceRow,
    spec: &SnapshotNode,
    target_def: Uuid,
    cfg: &SubConfig,
) -> std::result::Result<(DefRow, Uuid), spawn_error::Error> {
    let Some(def) = load_def(ex, target_def).await? else {
        return Err(spawn_error::Error::api(ApiError::new(
            422,
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            format!("子流程节点 {} 指向的流程定义不存在或已删除", spec.key),
        )));
    };
    if !def.status.eq_ignore_ascii_case("PUBLISHED") {
        return Err(spawn_error::Error::api(ApiError::new(
            422,
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            format!(
                "子流程节点 {} 指向的流程「{}」当前状态是 {}，只有已发布流程才能被调用",
                spec.key, def.flow_code, def.status
            ),
        )));
    }

    // PIN_CURRENT：同一 (父实例, 节点) 已经有子实例 → 复用它钉住的版本
    if cfg.version_policy == VersionPolicy::PinCurrent
        && let Some(version_id) = pinned_child_version(ex, parent.id, &spec.key).await?
        && let Some(row) = load_version_def(ex, version_id).await?
    {
        return Ok((row, version_id));
    }

    let Some(version_id) = def.current_version_id else {
        return Err(spawn_error::Error::api(ApiError::new(
            422,
            error_code::FLOW_SUBPROCESS_DEF_INVALID,
            format!(
                "子流程节点 {} 指向的流程「{}」没有当前发布版本",
                spec.key, def.flow_code
            ),
        )));
    };
    Ok((def, version_id))
}

/// 同一父实例同一节点已经发起过的子实例，它绑的版本号（`PIN_CURRENT` 用）。
async fn pinned_child_version(ex: &Transaction, parent_id: Uuid, node_key: &str) -> DbResult<Option<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT version_id FROM biz_flow_instance \
              WHERE parent_instance_id = ?::uuid AND parent_node_key = ? \
              ORDER BY started_at DESC LIMIT 1",
            vec![Value::from(&parent_id), Value::Text(node_key.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("version_id"),
    }
}

/// 按版本 id 读它所属的定义（版本号也一起读出来）。
async fn load_version_def(ex: &Transaction, version_id: Uuid) -> DbResult<Option<DefRow>> {
    let rows = ex
        .find_with_params(
            "SELECT d.flow_code, d.flow_name, d.biz_type, d.status, d.current_version_id, \
                    v.version_no AS version_no \
               FROM biz_flow_def_version v \
               JOIN biz_flow_def d ON d.id = v.def_id \
              WHERE v.id = ?::uuid AND d.deleted_at IS NULL",
            vec![Value::from(&version_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(DefRow {
        flow_code: row.get_typed("flow_code")?,
        flow_name: row.get_typed("flow_name")?,
        biz_type: row.get_typed("biz_type")?,
        status: row.get_typed("status")?,
        current_version_id: row.get_typed("current_version_id")?,
        version_no: row.get_typed("version_no")?,
    }))
}

/// 子实例标题（`biz_flow_instance.title` 是 `varchar(200)`）。
fn child_title(flow_name: &str, parent_title: &str) -> String {
    let raw = format!("{flow_name}（子流程）：{parent_title}");
    if raw.chars().count() <= 200 {
        return raw;
    }
    let mut trimmed: String = raw.chars().take(197).collect();
    trimmed.push_str("...");
    trimmed
}

/// 父 `form_data` → 子 `form_data`。
///
/// 取不到的路径**不写**（不塞 `null`）：子流程里「字段缺席」比「字段是 null」语义干净，
/// 条件表达式对缺席键走「缺值」分支，而不是被一个 null 骗过去。
fn map_form_data(parent: &Json, mapping: &BTreeMap<String, String>) -> Json {
    let mut out = Map::new();
    for (child_key, parent_path) in mapping {
        if let Some(value) = lookup_path(parent, parent_path) {
            out.insert(child_key.clone(), value.clone());
        } else {
            tracing::warn!(
                child_key = %child_key,
                parent_path = %parent_path,
                "子流程入参映射：父表单里没有这个路径（按缺值处理）"
            );
        }
    }
    Json::Object(out)
}

/// 按 `a.b.c` 取 JSON 值（数组下标不参与：表单键是扁平/嵌套对象）。
fn lookup_path<'a>(value: &'a Json, path: &str) -> Option<&'a Json> {
    let mut current = value;
    for segment in path.split('.') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

/// 子实例通过后，把子表单的值写回父表单（`config.sub.result_mapping`）。
///
/// 键 = 父表单路径，值 = 子表单路径；用 `jsonb_set` 原地更新（不整块重写 `form_data`，
/// 免得把父流程后来补的字段冲掉）。重复回调只会写入相同的值 —— 天然幂等。
pub async fn apply_result_mapping(
    ex: &Transaction,
    parent_id: Uuid,
    child_id: Uuid,
    mapping: &BTreeMap<String, String>,
) -> DbResult<usize> {
    if mapping.is_empty() {
        return Ok(0);
    }
    let rows = ex
        .find_with_params(
            "SELECT form_data::text AS form_text FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&child_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(0);
    };
    let form_text: String = row.get_typed("form_text")?;
    let child_form: Json = serde_json::from_str(&form_text).unwrap_or(Json::Null);

    let mut written = 0usize;
    for (parent_path, child_path) in mapping {
        let Some(value) = lookup_path(&child_form, child_path) else {
            tracing::warn!(
                child_id = %child_id,
                child_path = %child_path,
                parent_path = %parent_path,
                "子流程结果映射：子表单里没有这个路径（跳过）"
            );
            continue;
        };
        ex.execute_with_params(
            "UPDATE biz_flow_instance \
                SET form_data = jsonb_set(form_data, string_to_array(?, '.'), ?::jsonb, true), \
                    updated_at = now() \
              WHERE id = ?::uuid",
            vec![
                Value::Text(parent_path.clone()),
                Value::Json(value.to_string()),
                Value::from(&parent_id),
            ],
        )
        .await?;
        written += 1;
    }
    Ok(written)
}

// ==================== 结算回调（父子实例） ====================

/// 子实例落终态后的**父侧回调**（handler 在事务提交之后调用）。
///
/// 与 [`super::task_bridge::on_instance_settled`] 同款：
/// 失败只 `warn`（子流程的结论已经落库，不能因为它把子单回滚），
/// 非终态 / 不是子实例 / 已经回调过 → 静默返回。
///
/// # 成功与失败的口径
///
/// | 子实例状态 | `fail_action=ABORT` | `fail_action=CONTINUE` |
/// |---|---|---|
/// | `APPROVED` | 父节点 `APPROVED` → 继续 | 同左 |
/// | `REJECTED` / `CANCELLED` / `WITHDRAWN` | 父节点 `REJECTED` + **父实例 `REJECTED` 终止** | 父节点 `APPROVED` → 继续 |
pub async fn on_instance_settled(instance_id: Option<Uuid>, status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    if !is_terminal(status) {
        return None;
    }
    let parent_id = match db()
        .transaction(|tx| async move { engine::resume_parent_after_subprocess(&tx, instance_id).await })
        .await
    {
        Ok(parent_id) => parent_id,
        Err(error) => {
            tracing::warn!(
                child_instance_id = %instance_id,
                %error,
                "子流程结算回调父流程失败（子单结论已落库，不回滚）"
            );
            None
        }
    };
    if let Some(parent_id) = parent_id {
        tracing::info!(
            child_instance_id = %instance_id,
            parent_instance_id = %parent_id,
            child_status = %status,
            "子实例结算已回调父流程"
        );
    }
    parent_id
}

/// 是否终态（子实例跑到这里才回调父流程）。
pub fn is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "APPROVED" | "REJECTED" | "CANCELLED" | "WITHDRAWN"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    // ---------- ① 配置解析：默认值 / 形状 / 未知键 ----------

    #[test]
    fn sub_config_defaults_are_pin_current_and_abort() {
        let cfg = parse_config(&json!(null)).expect("null 是合法的最简配置");
        assert_eq!(cfg.version_policy, VersionPolicy::PinCurrent);
        assert_eq!(cfg.fail_action, FailAction::Abort);
        assert!(cfg.param_mapping.is_empty());
        assert!(!cfg.spawn_task);

        let empty = parse_config(&json!({})).expect("空对象合法");
        assert_eq!(empty, cfg);
    }

    #[test]
    fn sub_config_rejects_unknown_key_and_bad_enum() {
        let typo = parse_config(&json!({ "fail_actoin": "ABORT" })).expect_err("拼错的键必须报错");
        assert_eq!(typo.code, error_code::FLOW_SUBPROCESS_DEF_INVALID);
        assert!(typo.message.contains("fail_actoin"), "{}", typo.message);

        let bad = parse_config(&json!({ "version_policy": "LATEST" })).expect_err("未知策略");
        assert_eq!(bad.code, error_code::FLOW_SUBPROCESS_DEF_INVALID);

        let bad_spawn = parse_config(&json!({ "spawn_task": "yes" })).expect_err("非布尔");
        assert_eq!(bad_spawn.code, error_code::FLOW_SUBPROCESS_DEF_INVALID);
    }

    #[test]
    fn sub_config_parses_mappings_and_enums_case_insensitively() {
        let cfg = parse_config(&json!({
            "version_policy": "follow_latest",
            "fail_action": "continue",
            "param_mapping": { "amount": "expense.amount", "dept": "dept_id" },
            "result_mapping": { "budget_approved": "approved" },
            "spawn_task": true
        }))
        .expect("合法配置");
        assert_eq!(cfg.version_policy, VersionPolicy::FollowLatest);
        assert_eq!(cfg.fail_action, FailAction::Continue);
        assert_eq!(
            cfg.param_mapping.get("amount").map(String::as_str),
            Some("expense.amount")
        );
        assert_eq!(
            cfg.result_mapping.get("budget_approved").map(String::as_str),
            Some("approved")
        );
        assert!(cfg.spawn_task);
    }

    // ---------- ② 表单映射 ----------

    #[test]
    fn form_mapping_reads_dotted_paths_and_skips_missing() {
        let parent = json!({ "expense": { "amount": 1200.5 }, "dept_id": "d-1" });
        let mut mapping = BTreeMap::new();
        mapping.insert("amount".to_string(), "expense.amount".to_string());
        mapping.insert("dept".to_string(), "dept_id".to_string());
        mapping.insert("nope".to_string(), "expense.missing".to_string());
        let mapped = map_form_data(&parent, &mapping);
        assert_eq!(mapped["amount"], json!(1200.5));
        assert_eq!(mapped["dept"], json!("d-1"));
        assert!(mapped.get("nope").is_none(), "缺值不写 null");
    }

    // ---------- ③ 自递归 / 深度超限（R27 的纯函数半边） ----------

    #[test]
    fn nesting_rejects_self_recursion() {
        let mut children = BTreeMap::new();
        children.insert(u(1), vec![u(1)]);
        match nesting_depth(u(1), &children).expect_err("自递归必须被拒") {
            NestingProblem::Cycle(path) => {
                assert_eq!(path.first().copied(), Some(u(1)));
                assert_eq!(path.last().copied(), Some(u(1)));
            }
            other => panic!("应为环，实际 {other:?}"),
        }
    }

    #[test]
    fn nesting_rejects_indirect_cycle() {
        let mut children = BTreeMap::new();
        children.insert(u(1), vec![u(2)]);
        children.insert(u(2), vec![u(3)]);
        children.insert(u(3), vec![u(1)]);
        match nesting_depth(u(1), &children).expect_err("间接成环必须被拒") {
            NestingProblem::Cycle(path) => {
                assert_eq!(path.len(), 4, "1→2→3→1 的闭环路径：{path:?}")
            }
            other => panic!("应为环，实际 {other:?}"),
        }
    }

    #[test]
    fn nesting_allows_three_levels_and_rejects_four() {
        // 1 → 2 → 3 → 4 ⇒ 最深层 depth = 3（合法）
        let mut ok = BTreeMap::new();
        ok.insert(u(1), vec![u(2)]);
        ok.insert(u(2), vec![u(3)]);
        ok.insert(u(3), vec![u(4)]);
        assert_eq!(nesting_depth(u(1), &ok).expect("3 层合法"), 3);

        // 1 → 2 → 3 → 4 → 5 ⇒ depth = 4 > MAX_NEST_DEPTH
        let mut deep = ok.clone();
        deep.insert(u(4), vec![u(5)]);
        match nesting_depth(u(1), &deep).expect_err("4 层必须被拒") {
            NestingProblem::TooDeep { depth, .. } => assert_eq!(depth, 4),
            other => panic!("应为过深，实际 {other:?}"),
        }
    }

    #[test]
    fn nesting_handles_branching_graphs() {
        // 1 → {2, 3}, 2 → 4, 3 → 5；最深 2 层
        let mut children = BTreeMap::new();
        children.insert(u(1), vec![u(2), u(3)]);
        children.insert(u(2), vec![u(4)]);
        children.insert(u(3), vec![u(5)]);
        assert_eq!(nesting_depth(u(1), &children).expect("分叉也合法"), 2);
    }

    #[test]
    fn terminal_status_whitelist() {
        for status in ["APPROVED", "REJECTED", "CANCELLED", "WITHDRAWN", "approved"] {
            assert!(is_terminal(status), "{status} 应算终态");
        }
        for status in ["PENDING", "", "DRAFT"] {
            assert!(!is_terminal(status), "{status} 不应触发父回调");
        }
    }

    #[test]
    fn child_title_is_clipped_to_column_width() {
        let long = "单".repeat(300);
        let title = child_title("预算调整", &long);
        assert_eq!(title.chars().count(), 200);
        assert!(title.ends_with("..."));
    }
}
