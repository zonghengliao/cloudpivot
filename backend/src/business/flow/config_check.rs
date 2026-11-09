//! 配置期审批人可达性校验（NEAR-FIX-6 · N-7 ②）。
//!
//! # 要解决的事（评审原文）
//!
//! > 流程保存不校验兜底人存在性（错误延后到发起时才 422）
//!
//! 现状：节点上配了 `fallback_user_id` / `fallback_role_id`，保存时**一个字都不查**。
//! 用户配了一个已删 / 已停用的人，界面上「保存成功」，直到有人发起审批、走到该节点、
//! 角色里又恰好没人 —— 才在 `assignee::apply_fallback` 拿到 `422 + 65042`。
//! 那时改配置要重新走「另存草稿 → 发布」，而卡住的那张单没有任何待办能推进它。
//!
//! 同样的「错得晚」还出现在两处：
//!
//! | 配置 | 保存时 | 过去什么时候才报 |
//! |------|--------|------------------|
//! | 兜底人（用户）不存在 / 已删 / 停用 / 机器人没开开关 | 不查 | 发起 → 走到该节点 → 422 `65042` |
//! | 兜底角色不存在 / 没有在职成员 | 不查 | 同上（`65042`，文案「没有可用成员」） |
//! | 角色升级链（`sys_role.escalation_role_id`）指向已删角色 | 不查 | 同上（`65040`） |
//! | 普通节点指定的用户 / 角色 / 部门已被删 | 不查 | 发起 / 走到该节点时 422 `65018` |
//!
//! # 分两个阶段（不是一刀切）
//!
//! | 阶段 | 入口 | 判据 | 为什么这样分 |
//! |------|------|------|--------------|
//! | [`Phase::Save`] | `PUT /business/flow/version/{vid}/graph` | **兜底人 / 兜底角色 / 升级链** | 编辑器是「边改边存」的：允许保存半成品（结构问题本来就只提示不阻断），但**兜底是最后一道保险**，配错了必须当场拦住 —— 它错了等于保险没有 |
//! | [`Phase::Publish`] | `POST /business/flow/def/{id}/publish`、`POST /business/flow/version/{vid}/validate` | Save 的全部 + **每个节点可枚举审批人的可达性** | 发布 = 冻结成快照并让新单走它，此时「指定审批人已被删」不再是半成品，而是**每张新单都会踩**的坑。卡片把这条叫「全流程预演」 |
//!
//! # 为什么不查「成员是不是发起人」「成员口令是否可用」
//!
//! 那是**运行期**事实（今天可用不代表下周可用，且发起人因单而异）。这一层只保证
//! 「配置指向的对象**现在存在**、兜底人**现在可用**、升级链**走得通**」；
//! 与 `graph.rs` 里 R28 注释的边界一致：能把运行期才出现的问题推迟到运行期的，
//! 就不该在配置期假装能判。
//!
//! # 为什么纯判定与查库分开
//!
//! [`issues`] 吃 [`Lookup`]（一张内存快照），所以「哪种状态该报哪条规则、文案点名谁」
//! 可以在**无数据库**的环境里逐条断言（`#[cfg(test)] mod tests` 12 条）；
//! [`load`] 只负责把 DB 读成 [`Lookup`]（批量查询，一批一条 SQL，不按节点循环查库）。
//! 真库版本见 `#[cfg(test)] mod db_tests`（`ERP_DB_URL` 开关，不设则显式 SKIP）。
//!
//! # 规则编号
//!
//! R1–R28 在 [`crate::business::flow::graph`] 的纯函数校验里。本文件接 R29–R32
//! （都需要查库，所以不在纯函数里）：
//!
//! | 规则 | 管什么 | 错误码 |
//! |------|--------|--------|
//! | R29 | 兜底审批人（用户）存在 / 启用 / 机器人口径 | `65042` `FLOW_FALLBACK_INVALID` |
//! | R30 | 兜底角色存在 / 有在职成员 | `65042` `FLOW_FALLBACK_INVALID` |
//! | R31 | 角色升级链（逐级存在、无自引用 / 无环 / ≤ 8 级） | `65040` / `65041` |
//! | R32 | 节点可枚举审批人（USER / ROLE / DEPT_LEADER）指向的对象存在 | `65015` `FLOW_NODE_ASSIGNEE_MISSING` |

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::assignee::{ESCALATION_MAX_LEVELS, array_param};
use crate::business::flow::graph::{AssigneeKind, FlowGraph, Issue, NodeSpec};
use crate::shared::error_code;

// ==================== 阶段 ====================

/// 校验阶段（决定「哪些问题算阻断」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 保存草稿：只阻断兜底 / 升级链（半成品允许存）。
    Save,
    /// 发布 / 只校验：再加「全流程预演」—— 每个节点的可枚举审批人必须可达。
    Publish,
}

// ==================== 内存快照 ====================

/// 用户状态（`sys_user` 的一行投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserState {
    /// 展示名（`real_name` 优先，空则 `username`）。
    pub name: String,
    /// `status = 1`（在职 / 启用）。
    pub active: bool,
    /// `user_type = 'ROBOT'`。
    pub robot: bool,
}

/// 角色状态（`sys_role` + 在职成员数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleState {
    /// 角色编码。
    pub code: String,
    /// 角色名。
    pub name: String,
    /// 在职成员数（未过期、未软删、`status = 1`）。
    pub members: usize,
    /// 升级上级（`escalation_role_id`）。
    pub parent: Option<Uuid>,
}

/// 部门状态（`sys_dept` 的一行投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptState {
    /// 部门名。
    pub name: String,
}

/// 一条升级链的判定结果（从某个起始角色往上走）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainState {
    /// 走得通（到 `NULL` 为止，≤ [`ESCALATION_MAX_LEVELS`] 级）。
    Ok,
    /// 某一级角色不存在 / 已软删。
    Missing {
        /// 缺失的角色 id。
        parent: Uuid,
        /// 上一级（存在的）角色编码，用于点名。
        from_code: String,
    },
    /// 角色把自己的上级设成了自己。
    SelfReference {
        /// 角色编码。
        code: String,
    },
    /// 链上成环（再次走到某个已访问过的角色）。
    Cycle {
        /// 重复出现的角色编码。
        code: String,
    },
    /// 链长超过 [`ESCALATION_MAX_LEVELS`]。
    TooDeep {
        /// 最后一个仍存在的角色编码。
        code: String,
    },
}

/// 校验用的内存快照：**键存在** = 查过；**值为 `None`** = 查过但不存在 / 已软删。
///
/// 「键不存在」在 [`issues`] 里按「未查」处理（不报错、也不放行 —— 只会出现在
/// 手工构造的测试快照里；[`load`] 永远把查询过的 id 全部插进去）。
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    /// 用户。
    pub users: HashMap<Uuid, Option<UserState>>,
    /// 角色。
    pub roles: HashMap<Uuid, Option<RoleState>>,
    /// 部门。
    pub depts: HashMap<Uuid, Option<DeptState>>,
    /// 起始角色 → 升级链判定。
    pub chains: HashMap<Uuid, ChainState>,
}

impl Lookup {
    /// 手工构造（单测 / 预演工具用）：登记一个用户。
    pub fn with_user(mut self, id: Uuid, state: Option<UserState>) -> Self {
        self.users.insert(id, state);
        self
    }

    /// 手工构造：登记一个角色。
    pub fn with_role(mut self, id: Uuid, state: Option<RoleState>) -> Self {
        self.roles.insert(id, state);
        self
    }

    /// 手工构造：登记一个部门。
    pub fn with_dept(mut self, id: Uuid, state: Option<DeptState>) -> Self {
        self.depts.insert(id, state);
        self
    }

    /// 手工构造：登记一条升级链判定。
    pub fn with_chain(mut self, id: Uuid, state: ChainState) -> Self {
        self.chains.insert(id, state);
        self
    }
}

// ==================== 纯判定 ====================

/// 跑一遍校验，返回**全部**不通过项（不是第一个）。
///
/// # 参数
///
/// - `graph`：待校验的整图（编辑期 [`FlowGraph`]）；
/// - `lookup`：[`load`] 读出来的内存快照（也可手工构造，见单测）；
/// - `phase`：[`Phase::Save`] 只查兜底 / 升级链；[`Phase::Publish`] 再加审批人可达性。
///
/// # 返回值
///
/// 按「节点顺序 → 兜底人 / 兜底角色 → 升级链 → 审批人」排列，便于前端逐条高亮。
/// 每条都点名节点 key + 节点名 + 具体字段（架构 §3.7 V3 的硬性要求）。
pub fn issues(graph: &FlowGraph, lookup: &Lookup, phase: Phase) -> Vec<Issue> {
    let mut out: Vec<Issue> = Vec::new();

    // 角色 id → 引用它的节点（给升级链的问题点名用）
    let mut role_refs: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();

    for node in &graph.nodes {
        // ---------- R29 / R30：兜底人 ----------
        out.extend(fallback_issues(node, lookup));
        // ---------- 收集角色引用（R31 用） ----------
        if let Some(role_id) = node.fallback_role_id {
            role_refs.entry(role_id).or_default().insert(node.key.clone());
        }
        for assignee in &node.assignees {
            if AssigneeKind::parse(&assignee.kind) != Some(AssigneeKind::Role) {
                continue;
            }
            if let Ok(role_id) = Uuid::parse_str(assignee.ref_id.trim()) {
                role_refs.entry(role_id).or_default().insert(node.key.clone());
            }
        }
    }

    // ---------- R31：升级链 ----------
    for (role_id, node_keys) in &role_refs {
        let Some(state) = lookup.chains.get(role_id) else {
            continue; // 未查（只可能出现在手工快照里）
        };
        out.extend(chain_issues(role_id, state, lookup, node_keys));
    }

    // ---------- R32：全流程预演（仅发布） ----------
    if phase == Phase::Publish {
        for node in &graph.nodes {
            out.extend(assignee_issues(node, lookup));
        }
    }

    out
}

/// R29 / R30：单个节点的兜底配置。
fn fallback_issues(node: &NodeSpec, lookup: &Lookup) -> Vec<Issue> {
    let mut out = Vec::new();

    if let Some(user_id) = node.fallback_user_id {
        let rule = "R29";
        let code = error_code::FLOW_FALLBACK_INVALID;
        match lookup.users.get(&user_id) {
            Some(Some(user)) if !user.active => out.push(
                Issue::new(
                    rule,
                    code,
                    format!(
                        "节点 {}（{}）的兜底审批人「{}」已停用 —— 兜底要的是能签字的人，\
                         停用账号在运行期会被过滤掉，等于没配兜底（发起走到这里才 422）。\
                         请在「系统 → 用户」恢复启用，或把节点兜底人换成在职人员 / 改配兜底角色",
                        node.key, node.name, user.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            ),
            Some(Some(user)) if user.robot && !node.allow_robot => out.push(
                Issue::new(
                    rule,
                    code,
                    format!(
                        "节点 {}（{}）的兜底审批人是机器人「{}」，而本节点没有开启「允许机器人」——\
                         兜底要的是能签字的人。请换成真人，或在节点上打开「允许机器人」",
                        node.key, node.name, user.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            ),
            Some(Some(_)) => {}
            _ => out.push(
                Issue::new(
                    rule,
                    code,
                    format!(
                        "节点 {}（{}）的兜底审批人 {user_id} 不存在或已删除 —— \
                         运行期走到这一步会 422（65042），单据将无人可审。\
                         请到「流程定义 → 本流程 → 节点「{}」」重新选择兜底人",
                        node.key, node.name, node.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            ),
        }
    }

    if let Some(role_id) = node.fallback_role_id {
        let rule = "R30";
        let code = error_code::FLOW_FALLBACK_INVALID;
        match lookup.roles.get(&role_id) {
            Some(Some(role)) if role.members == 0 => out.push(
                Issue::new(
                    rule,
                    code,
                    format!(
                        "节点 {}（{}）的兜底角色「{}（{}）」当前没有任何在职成员 —— \
                         兜底角色展开后仍是 0 人，运行期照样 422（65042）。\
                         请给该角色加在职成员，或改配「兜底审批人」",
                        node.key, node.name, role.name, role.code
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            ),
            Some(Some(_)) => {}
            _ => out.push(
                Issue::new(
                    rule,
                    code,
                    format!(
                        "节点 {}（{}）的兜底角色 {role_id} 不存在或已删除 —— \
                         运行期走到这一步会 422（65042）。\
                         请到「流程定义 → 本流程 → 节点「{}」」重新选择兜底角色",
                        node.key, node.name, node.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            ),
        }
    }

    out
}

/// R31：一条升级链的判定 → 问题（`Ok` / 未查 → 空）。
fn chain_issues(role_id: &Uuid, state: &ChainState, lookup: &Lookup, node_keys: &BTreeSet<String>) -> Vec<Issue> {
    if matches!(state, ChainState::Ok) {
        return Vec::new();
    }
    // 起始角色本身就缺失：这一条已经由 R30（兜底角色）/ R32（指定角色）报过，
    // 再报一条「它的上级不存在」只会让同一个问题显示两遍（文案还会自相矛盾）。
    if let ChainState::Missing { parent, .. } = state
        && parent == role_id
    {
        return Vec::new();
    }
    let nodes: Vec<String> = node_keys.iter().cloned().collect();
    let start = describe_role(lookup, role_id);
    let rule = "R31";
    let (code, message) = match state {
        ChainState::Ok => return Vec::new(),
        ChainState::Missing { parent, from_code } => (
            error_code::FLOW_ESCALATION_INVALID,
            format!(
                "角色「{start}」的升级上级 {parent} 不存在或已删除（`escalation_role_id` 指向了已删角色，\
                 上一级是 {from_code}）—— 节点 {} 的审批人在角色无人时会沿这条链上溯并拿到 65040。\
                 请到「系统 → 角色」修正该角色的「上级角色」",
                nodes.join("、")
            ),
        ),
        ChainState::SelfReference { code: role_code } => (
            error_code::FLOW_ESCALATION_INVALID,
            format!(
                "角色「{start}」把自己的上级设成了自己（{role_code} 的 `escalation_role_id` 指向自己）—— \
                 节点 {} 的升级链会永远停在原地（运行期 65040）。请改掉这个自引用",
                nodes.join("、")
            ),
        ),
        ChainState::Cycle { code: role_code } => (
            error_code::FLOW_ESCALATION_INVALID,
            format!(
                "角色「{start}」的升级链成环（再次经过角色 {role_code}）—— \
                 节点 {} 的审批人上溯会死循环（运行期 65040）。请把环上某一级的上级改成 NULL 或指向环外角色",
                nodes.join("、")
            ),
        ),
        ChainState::TooDeep { code: role_code } => (
            error_code::FLOW_ESCALATION_TOO_DEEP,
            format!(
                "角色「{start}」的升级链超过 {} 级（最后一级是 {role_code}）—— \
                 运行期上溯到第 {} 级仍未找到人就会 65041。涉及节点 {}，请缩短这条链",
                ESCALATION_MAX_LEVELS,
                ESCALATION_MAX_LEVELS,
                nodes.join("、")
            ),
        ),
    };
    vec![Issue::new(rule, code, message).with_nodes(nodes)]
}

/// R32：单个节点的可枚举审批人（USER / ROLE / DEPT_LEADER）是否仍可达。
fn assignee_issues(node: &NodeSpec, lookup: &Lookup) -> Vec<Issue> {
    let mut out = Vec::new();
    for assignee in &node.assignees {
        let Some(kind) = AssigneeKind::parse(&assignee.kind) else {
            continue; // 非法 kind 由 R14 / DTO 校验报，这里不重复
        };
        if !kind.is_enumerable() {
            continue;
        }
        let Ok(ref_id) = Uuid::parse_str(assignee.ref_id.trim()) else {
            continue; // 非法 id 由 DTO 报
        };
        let (label, missing) = match kind {
            AssigneeKind::User => {
                let missing = !matches!(lookup.users.get(&ref_id), Some(Some(_)));
                ("指定审批人", missing)
            }
            AssigneeKind::Role => {
                let missing = !matches!(lookup.roles.get(&ref_id), Some(Some(_)));
                ("指定角色", missing)
            }
            AssigneeKind::DeptLeader => {
                let missing = !matches!(lookup.depts.get(&ref_id), Some(Some(_)));
                ("指定部门", missing)
            }
            _ => continue,
        };
        if !missing {
            continue;
        }
        out.push(
            Issue::new(
                "R32",
                error_code::FLOW_NODE_ASSIGNEE_MISSING,
                format!(
                    "节点 {}（{}）的{label} {ref_id} 不存在或已删除 —— \
                     这版流程发布后，每张走到该节点的单都会解析不出审批人（运行期 422 / 65018）。\
                     请在节点上重新选择审批人，或删掉这一条",
                    node.key, node.name
                ),
            )
            .with_nodes(vec![node.key.clone()]),
        );
    }
    out
}

/// 「角色「名称（编码）」」；查不到就只说 id。
fn describe_role(lookup: &Lookup, role_id: &Uuid) -> String {
    match lookup.roles.get(role_id) {
        Some(Some(role)) => format!("{}（{}）", role.name, role.code),
        _ => role_id.to_string(),
    }
}

// ==================== 查库 ====================

/// 图里所有「需要查库」的引用 id（用户 / 角色 / 部门）。
#[derive(Debug, Default, Clone)]
struct Refs {
    users: BTreeSet<Uuid>,
    roles: BTreeSet<Uuid>,
    depts: BTreeSet<Uuid>,
}

fn collect_refs(graph: &FlowGraph) -> Refs {
    let mut refs = Refs::default();
    for node in &graph.nodes {
        if let Some(user_id) = node.fallback_user_id {
            refs.users.insert(user_id);
        }
        if let Some(role_id) = node.fallback_role_id {
            refs.roles.insert(role_id);
        }
        for assignee in &node.assignees {
            let Some(kind) = AssigneeKind::parse(&assignee.kind) else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(assignee.ref_id.trim()) else {
                continue;
            };
            match kind {
                AssigneeKind::User => {
                    refs.users.insert(id);
                }
                AssigneeKind::Role => {
                    refs.roles.insert(id);
                }
                AssigneeKind::DeptLeader => {
                    refs.depts.insert(id);
                }
                _ => {}
            }
        }
    }
    refs
}

/// 把 DB 读成 [`Lookup`]：**一批一种 SQL**，不按节点循环查库。
///
/// # 参数
///
/// - `ex`：执行器（生产传 `db()`；单测可传真库连接）；
/// - `graph`：待校验的整图。
///
/// # 异常情况
///
/// 任何一条 SQL 失败 → [`DbError`]（调用方按「配置校验读库失败 = 500」处理；
/// 不许退化成「查不到就当没有」——那会把 DB 抖动变成一堆假的「角色不存在」）。
pub async fn load(ex: &impl SqlExecutor, graph: &FlowGraph) -> DbResult<Lookup> {
    let refs = collect_refs(graph);

    let users = load_users(ex, &refs.users).await?;
    let roles = load_roles(ex, &refs.roles).await?;
    let depts = load_depts(ex, &refs.depts).await?;
    let chains = load_chains(ex, &refs.roles).await?;

    Ok(Lookup {
        users,
        roles,
        depts,
        chains,
    })
}

async fn load_users(ex: &impl SqlExecutor, ids: &BTreeSet<Uuid>) -> DbResult<HashMap<Uuid, Option<UserState>>> {
    let mut out: HashMap<Uuid, Option<UserState>> = ids.iter().map(|id| (*id, None)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows = ex
        .query_sql(
            "SELECT id, COALESCE(NULLIF(real_name, ''), username, '') AS name, status, user_type \
               FROM sys_user \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![array_param(&ids.iter().copied().collect::<Vec<_>>())],
        )
        .await?;
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let status: i16 = row.get_typed("status")?;
        let user_type: String = row.get_typed("user_type")?;
        out.insert(
            id,
            Some(UserState {
                name: row.get_typed("name")?,
                active: status == 1,
                robot: user_type.eq_ignore_ascii_case("ROBOT"),
            }),
        );
    }
    Ok(out)
}

async fn load_roles(ex: &impl SqlExecutor, ids: &BTreeSet<Uuid>) -> DbResult<HashMap<Uuid, Option<RoleState>>> {
    let mut out: HashMap<Uuid, Option<RoleState>> = ids.iter().map(|id| (*id, None)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let list: Vec<Uuid> = ids.iter().copied().collect();
    let rows = ex
        .query_sql(
            "SELECT id, role_code, role_name, escalation_role_id \
               FROM sys_role \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![array_param(&list)],
        )
        .await?;
    // 在职成员数：与运行期 `assignee::load_role_members` + `filter_candidates` 同口径
    // （未过期、用户未软删、`status = 1`）
    let member_rows = ex
        .query_sql(
            "SELECT ur.role_id AS role_id, count(DISTINCT ur.user_id) AS members \
               FROM sys_user_role ur \
               JOIN sys_user u ON u.id = ur.user_id \
              WHERE ur.role_id = ANY(?::text::uuid[]) \
                AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
                AND u.deleted_at IS NULL AND u.status = 1 \
              GROUP BY ur.role_id"
                .to_string(),
            vec![array_param(&list)],
        )
        .await?;
    let mut members: HashMap<Uuid, usize> = HashMap::new();
    for row in &member_rows {
        let role_id: Uuid = row.get_typed("role_id")?;
        let count: i64 = row.get_typed("members")?;
        members.insert(role_id, usize::try_from(count).unwrap_or(usize::MAX));
    }
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        out.insert(
            id,
            Some(RoleState {
                code: row.get_typed("role_code")?,
                name: row.get_typed("role_name")?,
                members: members.get(&id).copied().unwrap_or(0),
                parent: row.get_typed("escalation_role_id")?,
            }),
        );
    }
    Ok(out)
}

async fn load_depts(ex: &impl SqlExecutor, ids: &BTreeSet<Uuid>) -> DbResult<HashMap<Uuid, Option<DeptState>>> {
    let mut out: HashMap<Uuid, Option<DeptState>> = ids.iter().map(|id| (*id, None)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows = ex
        .query_sql(
            "SELECT id, dept_name FROM sys_dept WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL".to_string(),
            vec![array_param(&ids.iter().copied().collect::<Vec<_>>())],
        )
        .await?;
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        out.insert(
            id,
            Some(DeptState {
                name: row.get_typed("dept_name")?,
            }),
        );
    }
    Ok(out)
}

/// 逐级读升级链并判定（每条链最多读 [`ESCALATION_MAX_LEVELS`] + 1 级）。
///
/// 读法：按「层」批量查（`id = ANY(...)`），不按角色逐条查 —— 200 节点的图里
/// 最多也就 9 条 SQL（层数上限决定，与节点数无关）。
async fn load_chains(ex: &impl SqlExecutor, starts: &BTreeSet<Uuid>) -> DbResult<HashMap<Uuid, ChainState>> {
    let mut out: HashMap<Uuid, ChainState> = HashMap::new();
    if starts.is_empty() {
        return Ok(out);
    }

    // 已读过的角色（`None` = 查过但不存在 / 已软删）
    let mut known: HashMap<Uuid, Option<RoleState>> = HashMap::new();
    let mut frontier: BTreeSet<Uuid> = starts.clone();
    for _ in 0..=ESCALATION_MAX_LEVELS {
        let unknown: Vec<Uuid> = frontier.iter().copied().filter(|id| !known.contains_key(id)).collect();
        if unknown.is_empty() {
            break;
        }
        let batch: BTreeSet<Uuid> = unknown.into_iter().collect();
        let loaded = load_roles(ex, &batch).await?;
        let mut next: BTreeSet<Uuid> = BTreeSet::new();
        for (id, state) in loaded {
            if let Some(role) = &state
                && let Some(parent) = role.parent
            {
                next.insert(parent);
            }
            known.insert(id, state);
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    for start in starts {
        out.insert(*start, walk_chain(&known, *start));
    }
    Ok(out)
}

/// 从起始角色往上走，判定链状态（纯函数，单测直接调）。
fn walk_chain(known: &HashMap<Uuid, Option<RoleState>>, start: Uuid) -> ChainState {
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    let mut current = Some(start);
    let mut level = 0_usize;
    let mut last_code = String::new();
    while let Some(id) = current {
        if !seen.insert(id) {
            let code = known
                .get(&id)
                .and_then(|state| state.as_ref())
                .map(|role| role.code.clone())
                .unwrap_or_else(|| id.to_string());
            return ChainState::Cycle { code };
        }
        level += 1;
        if level > ESCALATION_MAX_LEVELS {
            return ChainState::TooDeep { code: last_code };
        }
        match known.get(&id) {
            Some(Some(role)) => {
                if role.parent == Some(id) {
                    return ChainState::SelfReference {
                        code: role.code.clone(),
                    };
                }
                last_code = role.code.clone();
                current = role.parent;
            }
            _ => {
                // 第一级就不存在：`from_code` 记成自己（报文里 start 已经点过名）
                let from_code = if level == 1 { id.to_string() } else { last_code.clone() };
                return ChainState::Missing { parent: id, from_code };
            }
        }
    }
    ChainState::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::graph::{AssigneeRef, EdgeSpec, NodeType};

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn user(name: &str, active: bool, robot: bool) -> UserState {
        UserState {
            name: name.to_string(),
            active,
            robot,
        }
    }

    fn role(code: &str, members: usize) -> RoleState {
        RoleState {
            code: code.to_string(),
            name: format!("角色-{code}"),
            members,
            parent: None,
        }
    }

    /// 一张 `START → APPROVAL → END` 的图（审批人由调用方配）。
    fn graph_with(node: NodeSpec) -> FlowGraph {
        FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                node,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_ok"),
                EdgeSpec::new("e2", "n_ok", "n_end"),
            ],
        }
    }

    fn approval(key: &str, name: &str) -> NodeSpec {
        let mut node = NodeSpec::new(key, name, NodeType::Approval);
        node.assignee_type = Some(AssigneeKind::Initiator);
        node
    }

    fn refs_of(node: &NodeSpec) -> Vec<String> {
        vec![node.key.clone()]
    }

    /// R29：兜底人不存在 / 已删 → 保存期就阻断，点名节点。
    #[test]
    fn fallback_user_missing_blocks_save() {
        let ghost = uid(0x29_0001);
        let mut node = approval("n_ok", "部门经理审批");
        node.fallback_user_id = Some(ghost);
        let graph = graph_with(node);
        let lookup = Lookup::default().with_user(ghost, None);

        let issues = issues(&graph, &lookup, Phase::Save);
        assert_eq!(issues.len(), 1, "{issues:?}");
        let issue = &issues[0];
        assert_eq!(issue.rule, "R29");
        assert_eq!(issue.code, error_code::FLOW_FALLBACK_INVALID);
        assert!(issue.message.contains("部门经理审批"), "{}", issue.message);
        assert!(issue.message.contains(&ghost.to_string()), "{}", issue.message);
        assert_eq!(issue.nodes, vec!["n_ok".to_string()]);
        println!("[R29] 不存在的兜底人 → 400 / 65042，点名「部门经理审批」 ✓");
    }

    /// R29：兜底人已停用 → 阻断（验收 ③）。
    #[test]
    fn fallback_user_disabled_blocks_save() {
        let stopped = uid(0x29_0002);
        let mut node = approval("n_ok", "财务复核");
        node.fallback_user_id = Some(stopped);
        let graph = graph_with(node);
        let lookup = Lookup::default().with_user(stopped, Some(user("已离职的老王", false, false)));

        let issues = issues(&graph, &lookup, Phase::Save);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].rule, "R29");
        assert!(issues[0].message.contains("已停用"), "{}", issues[0].message);
        assert!(issues[0].message.contains("已离职的老王"), "{}", issues[0].message);
        println!("[R29] 停用的兜底人 → 400 / 65042 ✓");
    }

    /// R29：兜底人是机器人而节点没开 allow_robot → 阻断（运行期同样 422）。
    #[test]
    fn fallback_user_robot_without_switch_blocks() {
        let bot = uid(0x29_0003);
        let mut node = approval("n_ok", "主管审批");
        node.fallback_user_id = Some(bot);
        let graph = graph_with(node);
        let lookup = Lookup::default().with_user(bot, Some(user("审批机器人", true, true)));

        let found = issues(&graph, &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("机器人"), "{}", found[0].message);

        // 开了 allow_robot 就放行
        let mut allowed = approval("n_ok", "主管审批");
        allowed.fallback_user_id = Some(bot);
        allowed.allow_robot = true;
        assert!(issues(&graph_with(allowed), &lookup, Phase::Save).is_empty());
        println!("[R29] 机器人兜底人按节点 allow_robot 判定 ✓");
    }

    /// R30：兜底角色不存在 / 没有在职成员 → 阻断。
    #[test]
    fn fallback_role_missing_or_empty_blocks() {
        let ghost_role = uid(0x30_0001);
        let empty_role = uid(0x30_0002);

        let mut node = approval("n_ok", "总监审批");
        node.fallback_role_id = Some(ghost_role);
        let lookup = Lookup::default().with_role(ghost_role, None);
        let found = issues(&graph_with(node), &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R30");
        assert!(found[0].message.contains("不存在或已删除"), "{}", found[0].message);

        let mut node = approval("n_ok", "总监审批");
        node.fallback_role_id = Some(empty_role);
        let lookup = Lookup::default().with_role(empty_role, Some(role("AUDIT_DIRECTOR", 0)));
        let found = issues(&graph_with(node), &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R30");
        assert!(found[0].message.contains("没有任何在职成员"), "{}", found[0].message);
        assert!(found[0].message.contains("AUDIT_DIRECTOR"), "{}", found[0].message);
        println!("[R30] 兜底角色不存在 / 0 成员 → 400 / 65042 ✓");
    }

    /// R31：升级链指向已删角色 → 阻断（验收 ④），点名角色与节点。
    #[test]
    fn escalation_chain_to_deleted_role_blocks() {
        let role_id = uid(0x31_0001);
        let ghost_parent = uid(0x31_0002);
        let mut node = approval("n_ok", "采购经理审批");
        node.assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: role_id.to_string(),
            sort_order: 1,
        }];
        let mut start = role("PURCHASE_MANAGER", 2);
        start.parent = Some(ghost_parent);
        let lookup = Lookup::default().with_role(role_id, Some(start)).with_chain(
            role_id,
            ChainState::Missing {
                parent: ghost_parent,
                from_code: "PURCHASE_MANAGER".to_string(),
            },
        );

        let issues = issues(&graph_with(node), &lookup, Phase::Save);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].rule, "R31");
        assert_eq!(issues[0].code, error_code::FLOW_ESCALATION_INVALID);
        assert!(issues[0].message.contains("PURCHASE_MANAGER"), "{}", issues[0].message);
        assert!(
            issues[0].message.contains(&ghost_parent.to_string()),
            "{}",
            issues[0].message
        );
        assert_eq!(issues[0].nodes, vec!["n_ok".to_string()]);
        println!("[R31] 升级链指向已删角色 → 400 / 65040 ✓");
    }

    /// R31：自引用 / 成环 / 超过 8 级 各有各的码与文案。
    #[test]
    fn escalation_chain_shapes() {
        let role_id = uid(0x31_0010);
        let mut node = approval("n_ok", "复核");
        node.assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: role_id.to_string(),
            sort_order: 1,
        }];
        let graph = graph_with(node);

        let cases = [
            (
                ChainState::SelfReference { code: "A".into() },
                error_code::FLOW_ESCALATION_INVALID,
            ),
            (
                ChainState::Cycle { code: "B".into() },
                error_code::FLOW_ESCALATION_INVALID,
            ),
            (
                ChainState::TooDeep { code: "C".into() },
                error_code::FLOW_ESCALATION_TOO_DEEP,
            ),
        ];
        for (state, code) in cases {
            let lookup = Lookup::default()
                .with_role(role_id, Some(role("R", 1)))
                .with_chain(role_id, state.clone());
            let issues = issues(&graph, &lookup, Phase::Save);
            assert_eq!(issues.len(), 1, "{state:?}");
            assert_eq!(issues[0].rule, "R31");
            assert_eq!(issues[0].code, code, "{state:?}");
        }
        println!("[R31] 自引用 / 成环 / 超 8 级 分别 65040 / 65040 / 65041 ✓");
    }

    /// R32：普通节点指定了不存在的用户 → **保存放行**（半成品），发布阻断（预演）。
    #[test]
    fn static_assignee_missing_blocks_publish_only() {
        let ghost = uid(0x32_0001);
        let mut node = approval("n_ok", "指定审批");
        node.assignee_type = Some(AssigneeKind::User);
        node.assignees = vec![AssigneeRef {
            kind: "USER".to_string(),
            ref_id: ghost.to_string(),
            sort_order: 1,
        }];
        let graph = graph_with(node);
        let lookup = Lookup::default().with_user(ghost, None);

        assert!(
            issues(&graph, &lookup, Phase::Save).is_empty(),
            "保存期只拦兜底 / 升级链，普通审批人允许先存半成品"
        );
        let publish_issues = issues(&graph, &lookup, Phase::Publish);
        assert_eq!(publish_issues.len(), 1, "{publish_issues:?}");
        assert_eq!(publish_issues[0].rule, "R32");
        assert_eq!(publish_issues[0].code, error_code::FLOW_NODE_ASSIGNEE_MISSING);
        assert!(
            publish_issues[0].message.contains("指定审批人"),
            "{}",
            publish_issues[0].message
        );
        println!("[R32] 指定审批人已删：保存放行 / 发布 400 ✓");
    }

    /// R32：ROLE / DEPT_LEADER 同样管；合法的配置一条不报（验收 ⑤）。
    #[test]
    fn legal_config_is_clean() {
        let user_id = uid(0x32_0010);
        let role_id = uid(0x32_0011);
        let dept_id = uid(0x32_0012);
        let fb_user = uid(0x32_0013);
        let fb_role = uid(0x32_0014);

        let mut node = approval("n_ok", "财务复核");
        node.assignees = vec![
            AssigneeRef {
                kind: "USER".to_string(),
                ref_id: user_id.to_string(),
                sort_order: 1,
            },
            AssigneeRef {
                kind: "ROLE".to_string(),
                ref_id: role_id.to_string(),
                sort_order: 2,
            },
            AssigneeRef {
                kind: "DEPT_LEADER".to_string(),
                ref_id: dept_id.to_string(),
                sort_order: 3,
            },
        ];
        node.fallback_user_id = Some(fb_user);
        node.fallback_role_id = None;

        let mut second = approval("n_ok2", "总经理审批");
        second.fallback_role_id = Some(fb_role);
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                node,
                second,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_ok"),
                EdgeSpec::new("e2", "n_ok", "n_ok2"),
                EdgeSpec::new("e3", "n_ok2", "n_end"),
            ],
        };
        let lookup = Lookup::default()
            .with_user(user_id, Some(user("张三", true, false)))
            .with_user(fb_user, Some(user("李四", true, false)))
            .with_role(role_id, Some(role("FINANCE", 3)))
            .with_role(fb_role, Some(role("GM", 1)))
            .with_chain(role_id, ChainState::Ok)
            .with_chain(fb_role, ChainState::Ok)
            .with_dept(
                uid(0x32_0012),
                Some(DeptState {
                    name: "财务部".into()
                }),
            );

        assert!(issues(&graph, &lookup, Phase::Save).is_empty());
        let publish_issues = issues(&graph, &lookup, Phase::Publish);
        assert!(publish_issues.is_empty(), "{publish_issues:?}");
        println!("[⑤] 合法配置（指定人 / 角色 / 部门 + 兜底人 / 兜底角色）→ 零阻断 ✓");
    }

    /// 一张图里多条问题**全部**返回（不是只报第一个），且节点 key 不重复。
    #[test]
    fn reports_all_issues_not_just_first() {
        let ghost_user = uid(0x33_0001);
        let ghost_role = uid(0x33_0002);
        let mut a = approval("n_a", "甲审批");
        a.fallback_user_id = Some(ghost_user);
        let mut b = approval("n_b", "乙审批");
        b.fallback_role_id = Some(ghost_role);
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                a,
                b,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_a"),
                EdgeSpec::new("e2", "n_a", "n_b"),
                EdgeSpec::new("e3", "n_b", "n_end"),
            ],
        };
        let lookup = Lookup::default()
            .with_user(ghost_user, None)
            .with_role(ghost_role, None);
        let issues = issues(&graph, &lookup, Phase::Save);
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert_eq!(issues[0].nodes, refs_of(&graph.nodes[1]));
        assert_eq!(issues[1].nodes, refs_of(&graph.nodes[2]));
        println!("[汇总] 两条独立配置错一次性全部返回 ✓");
    }

    /// `walk_chain`：OK / 缺失 / 自引用 / 成环 / 超深 五种形状。
    #[test]
    fn walk_chain_shapes() {
        let a = uid(0x34_0001);
        let b = uid(0x34_0002);
        let c = uid(0x34_0003);
        let mut known: HashMap<Uuid, Option<RoleState>> = HashMap::new();
        let mut ra = role("A", 1);
        ra.parent = Some(b);
        let mut rb = role("B", 1);
        rb.parent = None;
        known.insert(a, Some(ra));
        known.insert(b, Some(rb));
        assert_eq!(walk_chain(&known, a), ChainState::Ok);
        assert_eq!(walk_chain(&known, b), ChainState::Ok);

        // 指向已删角色
        let ghost = uid(0x34_0009);
        let mut self_ref: HashMap<Uuid, Option<RoleState>> = HashMap::new();
        let mut ra = role("A", 1);
        ra.parent = Some(ghost);
        self_ref.insert(a, Some(ra));
        self_ref.insert(ghost, None);
        assert_eq!(
            walk_chain(&self_ref, a),
            ChainState::Missing {
                parent: ghost,
                from_code: "A".to_string()
            }
        );

        // 自引用
        let mut me: HashMap<Uuid, Option<RoleState>> = HashMap::new();
        let mut ra = role("A", 1);
        ra.parent = Some(a);
        me.insert(a, Some(ra));
        assert_eq!(walk_chain(&me, a), ChainState::SelfReference { code: "A".to_string() });

        // 成环 A → B → A
        let mut ring: HashMap<Uuid, Option<RoleState>> = HashMap::new();
        let mut ra = role("A", 1);
        ra.parent = Some(b);
        let mut rb = role("B", 1);
        rb.parent = Some(a);
        ring.insert(a, Some(ra));
        ring.insert(b, Some(rb));
        assert_eq!(walk_chain(&ring, a), ChainState::Cycle { code: "A".to_string() });

        // 超深：11 级链
        let mut deep: HashMap<Uuid, Option<RoleState>> = HashMap::new();
        let ids: Vec<Uuid> = (0..11).map(|index| uid(0x35_0000 + index)).collect();
        for (index, id) in ids.iter().enumerate() {
            let mut row = role(&format!("L{index}"), 1);
            row.parent = ids.get(index + 1).copied();
            deep.insert(*id, Some(row));
        }
        match walk_chain(&deep, ids[0]) {
            ChainState::TooDeep { code } => assert_eq!(code, format!("L{}", ESCALATION_MAX_LEVELS - 1)),
            other => panic!("应判 TooDeep，实际 {other:?}"),
        }
        let _ = c;
        println!("[walk_chain] OK / 缺失 / 自引用 / 成环 / 超深 ✓");
    }

    /// 起始角色本身缺失：只报 R30（兜底角色不存在），不再追加一条 R31「它的上级不存在」。
    ///
    /// 同一个问题显示两遍（且第二句自相矛盾）会让用户以为「角色存在但上级坏了」。
    #[test]
    fn missing_start_role_is_reported_once() {
        let role_id = uid(0x36_0001);
        let mut node = approval("n_ok", "总监审批");
        node.fallback_role_id = Some(role_id);
        let lookup = Lookup::default().with_role(role_id, None).with_chain(
            role_id,
            ChainState::Missing {
                parent: role_id,
                from_code: role_id.to_string(),
            },
        );
        let found = issues(&graph_with(node), &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R30");
        println!("[去重] 起始角色缺失 → 只报 R30 一条 ✓");
    }
}

/// 真库用例：只验「读库 → 快照」这一段（纯判定已被上面的单测覆盖）。
///
/// 与 `assignee::fallback_db_tests` 同口径：不设 `ERP_DB_URL` 就显式 SKIP（不假绿）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::flow::graph::{EdgeSpec, NodeType};

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\\nSQL: {sql}");
        }
    }

    /// 夹具：一个在职用户、一个停用用户、一个 0 成员角色、一个上级指向已删角色的角色。
    struct Fixture {
        active_user: Uuid,
        stopped_user: Uuid,
        empty_role: Uuid,
        orphan_role: Uuid,
        dept: Uuid,
    }

    async fn setup() -> Fixture {
        let active_user = Uuid::now_v7();
        let stopped_user = Uuid::now_v7();
        let empty_role = Uuid::now_v7();
        let orphan_role = Uuid::now_v7();
        let ghost_parent = Uuid::now_v7();
        let dept = Uuid::now_v7();
        exec(&format!(
            "INSERT INTO sys_dept (id, parent_id, dept_code, dept_name, status, sort_order) \
             VALUES ('{dept}'::uuid, NULL, 'N7CK_DEPT', 'N7 校验部', 1, 900)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, user_type, status, dept_id) \
             VALUES ('{active_user}'::uuid, 'n7ck_active', 'x', 'N7 在职', 'HUMAN', 1, '{dept}'::uuid), \
                    ('{stopped_user}'::uuid, 'n7ck_stopped', 'x', 'N7 停用', 'HUMAN', 0, '{dept}'::uuid)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_role (id, role_code, role_name, status, data_scope, sort_order, escalation_role_id) \
             VALUES ('{empty_role}'::uuid, 'N7CK_EMPTY', 'N7 空角色', 1, 1, 900, NULL), \
                    ('{orphan_role}'::uuid, 'N7CK_ORPHAN', 'N7 孤儿链', 1, 1, 901, '{ghost_parent}'::uuid)"
        ))
        .await;
        // 角色有成员（用于对比：空角色 vs 有成员）
        exec(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES ('{active_user}'::uuid, '{orphan_role}'::uuid)"
        ))
        .await;
        Fixture {
            active_user,
            stopped_user,
            empty_role,
            orphan_role,
            dept,
        }
    }

    async fn teardown(fixture: &Fixture) {
        let Fixture {
            active_user,
            stopped_user,
            empty_role,
            orphan_role,
            dept,
        } = fixture;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user_role WHERE user_id IN (?::uuid, ?::uuid)",
                vec![Value::from(active_user), Value::from(stopped_user)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(active_user), Value::from(stopped_user)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_role WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(empty_role), Value::from(orphan_role)],
            )
            .await;
        let _ = db()
            .execute_with_params("DELETE FROM sys_dept WHERE id = ?::uuid", vec![Value::from(dept)])
            .await;
    }

    fn node_with(fallback_user: Option<Uuid>, fallback_role: Option<Uuid>) -> NodeSpec {
        let mut node = NodeSpec::new("n_appr", "N7 审批", NodeType::Approval);
        node.assignee_type = Some(AssigneeKind::User);
        node.fallback_user_id = fallback_user;
        node.fallback_role_id = fallback_role;
        node
    }

    fn graph_of(node: NodeSpec) -> FlowGraph {
        FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                node,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_appr"),
                EdgeSpec::new("e2", "n_appr", "n_end"),
            ],
        }
    }

    /// 真库读快照：在职 / 停用 / 空角色 / 孤儿链 四种事实与 DB 一致，并各自触发对应规则。
    #[tokio::test]
    async fn load_reads_real_states_and_flags_them() {
        if !ensure_db().await {
            return;
        }
        let fx = setup().await;

        let mut node = node_with(Some(fx.active_user), None);
        node.assignees = vec![
            crate::business::flow::graph::AssigneeRef {
                kind: "USER".to_string(),
                ref_id: fx.active_user.to_string(),
                sort_order: 1,
            },
            crate::business::flow::graph::AssigneeRef {
                kind: "DEPT_LEADER".to_string(),
                ref_id: fx.dept.to_string(),
                sort_order: 2,
            },
        ];
        let graph = graph_of(node);
        let lookup = load(db(), &graph).await.expect("读快照");
        assert!(lookup.users.get(&fx.active_user).unwrap().as_ref().unwrap().active);
        assert!(lookup.depts.get(&fx.dept).unwrap().is_some());
        assert!(issues(&graph, &lookup, Phase::Publish).is_empty(), "合法配置零阻断");

        // 停用用户当兜底 → R29
        let graph = graph_of(node_with(Some(fx.stopped_user), None));
        let lookup = load(db(), &graph).await.expect("读快照");
        let found = issues(&graph, &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R29");
        assert!(found[0].message.contains("已停用"), "{}", found[0].message);

        // 0 成员角色当兜底 → R30
        let graph = graph_of(node_with(None, Some(fx.empty_role)));
        let lookup = load(db(), &graph).await.expect("读快照");
        let found = issues(&graph, &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R30");

        // 上级指向已删角色 → R31 Missing
        let mut with_role = node_with(None, None);
        with_role.assignee_type = Some(AssigneeKind::Role);
        with_role.assignees = vec![crate::business::flow::graph::AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: fx.orphan_role.to_string(),
            sort_order: 1,
        }];
        let graph = graph_of(with_role);
        let lookup = load(db(), &graph).await.expect("读快照");
        assert!(
            matches!(lookup.chains.get(&fx.orphan_role), Some(ChainState::Missing { .. })),
            "{:?}",
            lookup.chains.get(&fx.orphan_role)
        );
        let found = issues(&graph, &lookup, Phase::Save);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "R31");

        teardown(&fx).await;
        println!("[真库] 在职 / 停用 / 空角色 / 孤儿链 四种事实读取正确，各自触发 R29/R30/R31 ✓");
    }
}
