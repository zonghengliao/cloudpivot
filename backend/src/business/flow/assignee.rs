//! 审批流 · 审批人解析（批次 D · D2）。
//!
//! 依据：卡面 t_3715c446「2. `flow/assignee.rs`」+
//! `docs/批次D-工作流审批-技术架构.md` §4.3（8 种来源 / 三层去重 / 冻结）、
//! §4.3.4（空审批人策略 X2）、§4.3.5（自审策略 X3）、§4.3.6（AI 审批人 D18）、
//! §13.3（坑 3：自审三种形态）/ §13.5（坑 5：角色里一个人都没有）。
//!
//! # 一句话
//!
//! **把「节点配置的审批人来源」翻译成「点名到人的待办行」**，并在这一步就把
//! 三层去重的第 1 层（按 `user_id` 去重）、自审策略、空审批人策略、ROBOT 过滤做完。
//!
//! # 8 种来源（`biz_flow_node.assignee_type` / 快照 `assignee_type`）
//!
//! | 来源 | 解析 | 空结果典型原因 |
//! |------|------|----------------|
//! | `USER` | `node_assignee` 里的用户 id | 用户被软删 / 停用 |
//! | `ROLE` | `sys_user_role` 展开 ∩ 未删未停 | 角色内 0 人 |
//! | `INITIATOR` | 发起人本人 | 不可能空 |
//! | `INITIATOR_SUPERIOR` | 发起人部门沿 `dept_path` 上溯 `superior_levels` 级的 `leader_id` | 上级部门没设负责人 |
//! | `DEPT_LEADER` | `node_assignee` 里指定部门的 `leader_id` | 该部门没设负责人 |
//! | `TASK_OWNER` | 关联任务 `owner_id`（仅 `biz_type=TASK_ACCEPT`） | 任务已删 |
//! | `TASK_CREATOR` | 关联任务 `creator_id` | 同上 |
//! | `TASK_ASSIGNER` | 关联任务 `assigner_id`（可空 → 空结果） | 自建任务的 `assigner_id` 为空 |
//! | `FORM_VAR` | `form_data[form_var_key]`（uuid 或 uuid 数组） | 发起人没填 / 填错 |
//! | `ADMIN` | `role_code = SUPER_ADMIN` 的所有人（`FALLBACK_ADMIN` 兜底用） | 不可能空 |
//! | `EXPRESSION` | `node.config.expression` 规则树求值 → 展开成上面 10 种来源之一（`expression.rs`） | 无规则命中 / 命中但查出 0 人 |
//!
//! `EXPRESSION`（批次 D · P1-A2）走 [`expression`] 的纯求值器：**首条命中即定**，
//! 命中后把 `then` 的目标翻译成 [`SourcePlan`] 再喂进**同一条** `load_plan` + [`finalize`] 管线
//! （三层去重 / 自审 / 空审批人 / ROBOT 过滤全部照旧，一行逻辑没改）。
//! 命中规则的 id 写进 [`Finalized::rule_id`] → `biz_flow_task.assignee_rule_id`（D-E5）；
//! `assignee_source` 仍写**底层来源**（`ROLE` / `USER` / …），不写 `EXPRESSION`（D-E5 注）。
//!
//! # 三层去重（D14，缺一不可）
//!
//! 1. **解析期**（本文件 [`finalize`]）：按 `user_id` 去重 —— 一个人可能同时是
//!    「角色成员」和「发起人主管」；只靠落库层，并发下会进两条待办（一个人投两票）。
//! 2. **落库期**（`engine.rs` + 库）：`uk_biz_flow_task_pending (instance_node_id, assignee_id)
//!    WHERE status='PENDING'`，23505 当幂等成功。
//! 3. **计票期**（`engine.rs`）：`COUNT(*)` 现算，**禁止 `+=`**。
//!
//! # 三个策略
//!
//! - `self_approval(ALLOW|SKIP|FORBID)`，默认 **SKIP**，且 **SKIP 要落一条 `SKIPPED` 待办行**
//!   （不是静默丢弃）—— 审批时间线要能看到「本来该他审，因为自审被跳过」。
//! - `empty_assignee(SKIP|ERROR|FALLBACK_ADMIN)`（卡面写作 `SKIP|FORBID|FALLBACK_ADMIN`，
//!   `FORBID` 按 `ERROR` 处理），默认 SKIP，**计入 `instance.auto_passed_nodes`**。
//! - `allow_robot`（D18）：**隐式**来源（ROLE / 主管 / 负责人 / 管理员）默认过滤 ROBOT；
//!   **显式 `USER`** 指定不受限（「人明确选了这个机器人」是授予行为）。
//!
//! # 降级链（P3-02 / ARCH-8 第 ② 层）
//!
//! 自审 SKIP 在「角色里只有制单人一个人」的小公司里必然把审批人剔光 —— 这不该等于
//! 「这张单发不出去」。所以 `SKIP` 之后还多一步（[`escalate`]）：
//! 沿 `sys_role.escalation_role_id` **逐级上溯**，每级重跑停用 / ROBOT / 自审过滤，
//! 找到第一个有可用成员的角色就把待办派给他们，并留痕 `escalated_from_role` +
//! `escalate_reason`（待办行上）。到顶仍无人时才回到 `empty_assignee` 策略
//! （本卡不兜底，第 ③ 层兜底人是 P3-03）。
//!
//! **链条是可配置数据，不是代码**：本文件只认 `escalation_role_id` 这个指针，
//! 不认识任何具体角色编码（决策 D5）。
//!
//! # 冻结（D13）
//!
//! 本文件的输出（[`Finalized`]）由 `engine.rs` 立刻写成 `biz_flow_task` 行
//! （`assignee_id` + `assignee_source` + `assignee_source_ref` + `assignee_name` 名字快照）。
//! **运行期不再重新解析**：角色成员变了、部门调了，在跑的单子审批人不变。

use std::collections::BTreeSet;

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;

use crate::business::flow::expression::{self, ResolveContext, ResolvedTarget};
use crate::business::flow::graph::{self, AssigneeKind, EvalContext, SnapshotNode};
use crate::business::flow::no_assignee;
use crate::shared::error_code;

// ==================== 策略枚举 ====================

/// 自审策略（架构 X3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfPolicy {
    /// 保留，待办打 `is_self_approval = true`（UI 显示「自审」标）。
    Allow,
    /// 剔除发起人，并落一条 `SKIPPED` 待办留痕（**默认**）。
    Skip,
    /// 422 + `65019`，整单回滚。
    Forbid,
}

impl SelfPolicy {
    /// 解析（未知值 → `Skip`，与 `biz_flow_node.self_approval` 的默认值一致）。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ALLOW" => SelfPolicy::Allow,
            "FORBID" => SelfPolicy::Forbid,
            _ => SelfPolicy::Skip,
        }
    }
}

/// 空审批人策略（架构 X2）。卡面写作 `FORBID`，库里是 `ERROR`，两者等价。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyPolicy {
    /// 节点 `SKIPPED`，`auto_passed_nodes + 1`，继续往下走（**默认**）。
    Skip,
    /// 422 + `65018`，整单回滚（财务 / 法务这类「宁可卡住也不能漏审」的节点用）。
    Error,
    /// 落一条 `assignee_source=ADMIN` 的待办给超管（至少有人能看到）。
    FallbackAdmin,
}

impl EmptyPolicy {
    /// 解析（`FORBID` 与 `ERROR` 同义；未知值 → `Skip`）。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ERROR" | "FORBID" => EmptyPolicy::Error,
            "FALLBACK_ADMIN" => EmptyPolicy::FallbackAdmin,
            _ => EmptyPolicy::Skip,
        }
    }
}

// ==================== 候选人 ====================

/// 一个「解析出来的候选人」（还没去重、还没过策略）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 用户 id。
    pub user_id: Uuid,
    /// 名字快照（`real_name` 为空时用 `username`）。
    pub name: String,
    /// 来源（写进 `biz_flow_task.assignee_source`）。
    pub source: AssigneeKind,
    /// 来源角色 / 部门 id（写进 `assignee_source_ref`）。
    pub source_ref: Option<Uuid>,
    /// 是否机器人（`sys_user.user_type = 'ROBOT'`）。
    pub is_robot: bool,
    /// 是否启用（`sys_user.status = 1`）；软删用户在 SQL 层就不返回。
    pub active: bool,
}

impl Candidate {
    /// 构造（测试与「显式用户」来源用）。
    pub fn new(user_id: Uuid, name: impl Into<String>, source: AssigneeKind) -> Self {
        Self {
            user_id,
            name: name.into(),
            source,
            source_ref: None,
            is_robot: false,
            active: true,
        }
    }

    /// 打上来源引用。
    pub fn with_ref(mut self, source_ref: Option<Uuid>) -> Self {
        self.source_ref = source_ref;
        self
    }
}

// ==================== 解析结果 ====================

/// 最终结果（`engine.rs` 直接照它落 `biz_flow_task` 行）。
///
/// **不 derive `Clone`**：`ApiError` 没有 `Clone`（它带 `internal_detail`），
/// 而把错误克隆一份去两个地方消费本身就是设计味道 —— 引擎里错误只走一条路。
#[derive(Debug, Default)]
pub struct Finalized {
    /// 应该落 `PENDING` 待办的人（已去重、已过滤 ROBOT、已剔除自审）。
    pub approvers: Vec<Candidate>,
    /// 应该落 `SKIPPED` 待办留痕的人（只有「自审 SKIP」会产生）。
    pub skipped: Vec<Candidate>,
    /// 是否因「无有效审批人」而自动通过（节点 `SKIPPED` + `auto_passed_nodes + 1`）。
    pub auto_pass: bool,
    /// 是否要回退到「平台管理员」（`empty_assignee = FALLBACK_ADMIN`）。
    pub fallback_admin: bool,
    /// 业务拒绝（`65019` 自审禁止 / `65018` 无审批人 / `65032` EXPRESSION 未实现）。
    pub error: Option<ApiError>,
    /// 表达式命中的规则 id（`EXPRESSION` 节点专属，写 `biz_flow_task.assignee_rule_id`，D-E5）。
    ///
    /// 静态来源、以及「命中规则但表达式没配 id」之外的一切情形都是 `None`。
    /// `otherwise` 命中时写 [`expression::OTHERWISE_RULE_ID`]。
    pub rule_id: Option<String>,
    /// 升级留痕：**原本该审**的角色编码（P3-02）。非空 = 这批判待办是沿升级链派下来的。
    ///
    /// 写 `biz_flow_task.escalated_from_role`（varchar(40)）—— 多个起始角色时用 `,` 连接。
    pub escalated_from_role: Option<String>,
    /// 升级原因：`SKIP_SELF_APPROVAL`（自审策略剔光）/ `NO_MEMBER`（角色 0 成员）。
    pub escalate_reason: Option<String>,
    /// 升级链走过的**全部上级角色**（含已找到人的那一级）；`None` = 没走升级链。
    ///
    /// 引擎用它组装「升级链已走完」的 422 报文与结构化诊断（P3-02）；
    /// 走通时它只是排障痕迹，不进业务表（业务表只留 [`Self::escalated_from_role`]）。
    pub escalation: Option<Escalation>,
    /// 兜底尝试的痕迹（P3-03）；`None` = 节点没配兜底人。
    pub fallback: Option<Fallback>,
    /// 派单规则留痕（写 `biz_flow_task.assigned_by_rule`，P3-03）。
    ///
    /// 只有「升级链 / 兜底人」这两条**降级路径**在这里定值（`ESCALATED` / `FALLBACK`）；
    /// 普通路径由引擎按候选来源补 `ROLE`（一个节点可能混着 ROLE + USER 来源，
    /// 只有引擎逐行看得到候选）。`None` = 由引擎决定。
    pub assigned_by_rule: Option<String>,
    /// 解析痕迹（写日志 + 详情页提示）。
    pub notes: Vec<String>,
}

impl Finalized {
    fn failed(error: ApiError, notes: Vec<String>) -> Self {
        Self {
            error: Some(error),
            notes,
            ..Self::default()
        }
    }

    /// 节点是否应当被跳过（无有效审批人）。
    pub fn node_is_skipped(&self) -> bool {
        self.auto_pass && self.error.is_none()
    }
}

/// 解析结论（含 trace）。
#[derive(Debug)]
pub struct Resolution {
    /// 最终结果。
    pub finalized: Finalized,
    /// 每个来源命中几人的痕迹（`ROLE=3` 这类），排障用。
    pub trace: Vec<String>,
}

// ==================== 来源计划（纯逻辑，可单测） ====================

/// 「这个节点该去哪张表查人」（把快照配置翻译成一次查询计划）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourcePlan {
    /// 指定用户 id 列表。
    Users(Vec<Uuid>),
    /// 指定角色 id 列表（要展开成员）。
    Roles(Vec<Uuid>),
    /// 发起人本人。
    Initiator,
    /// 发起人主管（上溯层数）。
    InitiatorSuperior(i16),
    /// 指定部门 id 列表（取部门负责人）。
    DeptLeaders(Vec<Uuid>),
    /// 关联任务负责人 / 创建人 / 分派人。
    TaskParty(TaskParty),
    /// 表单变量。
    FormVar(String),
    /// 平台超管。
    Admins,
    /// 不支持（`EXPRESSION` 等）—— 调用方返回 501。
    Unsupported(String),
}

/// 关联任务上的哪一方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskParty {
    /// `biz_task.owner_id`
    Owner,
    /// `biz_task.creator_id`
    Creator,
    /// `biz_task.assigner_id`
    Assigner,
}

/// 把快照节点翻译成查询计划（**不碰库**，纯函数 → 8 种来源可单测）。
pub fn plan_of(node: &SnapshotNode) -> SourcePlan {
    let raw = node
        .assignee_type
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    let kind = AssigneeKind::parse(&raw);
    match kind {
        Some(AssigneeKind::User) => SourcePlan::Users(ids_of(node, AssigneeKind::User)),
        Some(AssigneeKind::Role) => SourcePlan::Roles(ids_of(node, AssigneeKind::Role)),
        Some(AssigneeKind::Initiator) => SourcePlan::Initiator,
        Some(AssigneeKind::InitiatorSuperior) => {
            SourcePlan::InitiatorSuperior(node.superior_levels.clamp(1, 10))
        }
        Some(AssigneeKind::DeptLeader) => {
            SourcePlan::DeptLeaders(ids_of(node, AssigneeKind::DeptLeader))
        }
        Some(AssigneeKind::TaskOwner) => SourcePlan::TaskParty(TaskParty::Owner),
        Some(AssigneeKind::TaskCreator) => SourcePlan::TaskParty(TaskParty::Creator),
        Some(AssigneeKind::TaskAssigner) => SourcePlan::TaskParty(TaskParty::Assigner),
        Some(AssigneeKind::FormVar) => {
            SourcePlan::FormVar(node.form_var_key.clone().unwrap_or_default())
        }
        Some(AssigneeKind::Admin) => SourcePlan::Admins,
        // `EXPRESSION` 不走这里：求值需要 `EvalContext`（`plan_of` 是纯配置翻译，没有上下文），
        // 由 `resolve()` 的表达式分支单独处理（见 `expression_plans`）。
        // 这里留 `Unsupported` 是为了「单独调 plan_of」时不静默当空审批人。
        Some(AssigneeKind::Expression) => SourcePlan::Unsupported("EXPRESSION".to_string()),
        None => SourcePlan::Unsupported(if raw.is_empty() {
            "未配置审批人来源".to_string()
        } else {
            raw
        }),
    }
}

/// 取快照里某种来源的 `ref_id` 列表（`ref_id` 不是 UUID → 丢弃并留痕由调用方处理）。
fn ids_of(node: &SnapshotNode, kind: AssigneeKind) -> Vec<Uuid> {
    let want = kind.as_str();
    let mut out: Vec<Uuid> = node
        .assignees
        .iter()
        .filter(|assignee| assignee.assignee_type.eq_ignore_ascii_case(want))
        .filter_map(|assignee| Uuid::parse_str(assignee.ref_id.trim()).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 节点配置里的角色 id（诊断载荷用：`no_assignee` 要报「哪个角色没人」）。
///
/// 与 [`plan_of`] 走**同一个** [`ids_of`]，不另写一份解析：
/// 「预演算 3 个角色、真跑算 2 个」这种漂移只会在两份实现之间存在。
pub(crate) fn role_ids_of(node: &SnapshotNode) -> Vec<Uuid> {
    ids_of(node, AssigneeKind::Role)
}

/// 节点配置里的部门 id（`DEPT_LEADER` 来源）。
pub(crate) fn dept_ids_of(node: &SnapshotNode) -> Vec<Uuid> {
    ids_of(node, AssigneeKind::DeptLeader)
}

/// 节点配置里的用户 id（`USER` 来源）。
pub(crate) fn user_ids_of(node: &SnapshotNode) -> Vec<Uuid> {
    ids_of(node, AssigneeKind::User)
}

// ==================== 最终裁决（纯逻辑，可单测） ====================

/// 过滤结果（①–④ 之后、空审批人策略之前）。
///
/// P3-02 把「过滤」与「空审批人策略」拆开：升级链必须在**两步之间**插入
/// （「剔光 → 先找上级 → 上级也没有才按 empty_assignee 处置」）。
#[derive(Debug, Default)]
pub(crate) struct Approvable {
    /// 可以落 `PENDING` 的人。
    pub approvers: Vec<Candidate>,
    /// 自审 SKIP 被剔出去的人（落 `SKIPPED` 留痕）。
    pub skipped: Vec<Candidate>,
}

/// 把候选人合并、去重、过策略，得到「能签的人 / 被自审剔掉的人」
/// （三层去重的**第 1 层**，架构 D14）。
///
/// # 规则（顺序不可换）
///
/// 1. **停用用户剔除**（`status <> 1`）—— 「角色里的人都辞职了」是坑 5 的真实原因；
/// 2. **ROBOT 过滤**（D18）：`allow_robot = false` 时只保留**显式 `USER`** 来源的机器人；
/// 3. **按 `user_id` 去重**：优先级「显式 USER > 其它来源」，其余保持首次出现顺序
///    （一个人同时是「角色成员 + 发起人主管」→ 只生成 1 行待办）；
/// 4. **自审策略**：命中发起人时按 `ALLOW` / `SKIP`（落 SKIPPED 留痕）/ `FORBID`（`65019`）。
///
/// 第 ⑤ 步（空审批人策略）不在这里：见 [`finalize`] 与 [`apply_empty_policy`]。
pub(crate) fn filter_candidates(
    candidates: Vec<Candidate>,
    self_policy: SelfPolicy,
    allow_robot: bool,
    initiator: Uuid,
    notes: &mut Vec<String>,
) -> std::result::Result<Approvable, ApiError> {
    // ① 停用用户
    let mut alive: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.active {
            alive.push(candidate);
        } else {
            notes.push(format!("{} 已停用，已剔除", candidate.name));
        }
    }

    // ② ROBOT 过滤（D18）：隐式来源默认过滤；显式 USER 不受限
    let mut kept: Vec<Candidate> = Vec::with_capacity(alive.len());
    for candidate in alive {
        if candidate.is_robot && !allow_robot && candidate.source != AssigneeKind::User {
            notes.push(format!(
                "{} 是机器人且来源为 {}（隐式），allow_robot=false → 已过滤",
                candidate.name,
                candidate.source.as_str()
            ));
        } else {
            kept.push(candidate);
        }
    }

    // ③ 按 user_id 去重：显式 USER 优先（排序稳定，其余保持原顺序）
    kept.sort_by_key(|candidate| candidate.source != AssigneeKind::User);
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    let mut deduped: Vec<Candidate> = Vec::with_capacity(kept.len());
    for candidate in kept {
        if seen.insert(candidate.user_id) {
            deduped.push(candidate);
        } else {
            notes.push(format!(
                "{} 由多个来源命中（重复来源 {}），已去重",
                candidate.name,
                candidate.source.as_str()
            ));
        }
    }

    // ④ 自审策略
    let mut approvers: Vec<Candidate> = Vec::with_capacity(deduped.len());
    let mut skipped: Vec<Candidate> = Vec::new();
    for candidate in deduped {
        if candidate.user_id != initiator {
            approvers.push(candidate);
            continue;
        }
        match self_policy {
            SelfPolicy::Allow => {
                notes.push(format!(
                    "{} 是发起人本人，self_approval=ALLOW → 保留（自审标）",
                    candidate.name
                ));
                approvers.push(candidate);
            }
            SelfPolicy::Skip => {
                notes.push(format!(
                    "{} 是发起人本人，self_approval=SKIP → 落 SKIPPED 待办留痕（不静默丢弃）",
                    candidate.name
                ));
                skipped.push(candidate);
            }
            SelfPolicy::Forbid => {
                return Err(ApiError::new(
                    422,
                    error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
                    format!(
                        "审批人 {} 是发起人本人，本节点不允许自审（self_approval=FORBID）",
                        candidate.name
                    ),
                ));
            }
        }
    }

    Ok(Approvable { approvers, skipped })
}

/// 空审批人策略（`finalize` 的第 ⑤ 步）。
///
/// P3-02 之后它的调用点从 `finalize` 内部挪到「升级链之后」：先按配置找上级角色，
/// 找到人就不必走这里；到顶仍无人时才按 `empty_assignee` 处置（SKIP 自动通过 /
/// ERROR 报 `65018` / FALLBACK_ADMIN 回退超管）。
fn apply_empty_policy(finalized: &mut Finalized, empty_policy: EmptyPolicy) {
    if !finalized.approvers.is_empty() {
        return;
    }
    match empty_policy {
        EmptyPolicy::Skip => {
            finalized
                .notes
                .push("本节点没有有效审批人，empty_assignee=SKIP → 节点自动跳过并计数".to_string());
            finalized.auto_pass = true;
        }
        EmptyPolicy::Error => {
            finalized.error = Some(ApiError::new(
                422,
                error_code::FLOW_NO_ASSIGNEE,
                "本节点没有一个可用审批人（empty_assignee=ERROR）",
            ));
        }
        EmptyPolicy::FallbackAdmin => {
            finalized.notes.push(
                "本节点没有有效审批人，empty_assignee=FALLBACK_ADMIN → 回退给平台管理员"
                    .to_string(),
            );
            finalized.fallback_admin = true;
        }
    }
}

/// 把候选人合并、去重、过策略，得到「该落哪些待办」
/// （三层去重的**第 1 层**，架构 D14）。
///
/// 四个过滤步骤见 [`filter_candidates`]，第 ⑤ 步见 [`apply_empty_policy`]。
///
/// **P3-02 注**：生产路径（[`resolve`]）不再直接调本函数，而是分三步
/// 「过滤 → 升级链 → 空审批人策略」；本函数保留为「不做升级链」的组合入口
/// （单测与历史调用方），语义与 P3-02 之前逐字一致。
pub fn finalize(
    candidates: Vec<Candidate>,
    self_policy: SelfPolicy,
    empty_policy: EmptyPolicy,
    allow_robot: bool,
    initiator: Uuid,
) -> Finalized {
    let mut notes: Vec<String> = Vec::new();
    let filtered =
        match filter_candidates(candidates, self_policy, allow_robot, initiator, &mut notes) {
            Ok(filtered) => filtered,
            Err(error) => return Finalized::failed(error, notes),
        };
    let mut finalized = Finalized {
        approvers: filtered.approvers,
        skipped: filtered.skipped,
        notes,
        ..Finalized::default()
    };
    apply_empty_policy(&mut finalized, empty_policy);
    finalized
}

// ==================== 实例上下文 ====================

/// 解析审批人需要知道的「这张单是谁发的、挂在什么业务上」。
#[derive(Debug, Clone)]
pub struct InstanceCtx {
    /// 实例 id（抄送行 / 任务行冗余用）。
    pub instance_id: Uuid,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人部门（`instance.initiator_dept_id`）。
    pub initiator_dept_id: Option<Uuid>,
    /// 业务类型（`TASK_ACCEPT` 才能解析 `TASK_OWNER` / `TASK_CREATOR` / `TASK_ASSIGNER`）。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 表单数据（`FORM_VAR` 的取值作用域，发起时冻结）。
    pub form_data: Json,
}

// ==================== SQL 助手 ====================

/// 文本数组绑定（本栈驱动不支持 `Value::Array`，统一转 `{uuid,uuid}` 字面量 +
/// `?::text::uuid[]`；见 `business/shared/scope.rs` 文件头的实测）。
pub fn array_param(ids: &[Uuid]) -> Value {
    let body = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Value::Text(format!("{{{body}}}"))
}

fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

// ==================== DB 解析 ====================

// ==================== 表达式审批人（批次 D · P1-A2 / EXPRESSION DSL） ====================

/// 表达式求值后的「目标清单」（[`expression_plans`] 的产物）。
///
/// 纯内存结构：`RoleCodes` 还要调用方查一次 `sys_role` 的 code→id（§8.3 方案 B）。
#[derive(Debug, Clone, Default)]
pub struct ExpressionPlans {
    /// 命中的规则 id（无命中 → `None`；命中 `otherwise` → `expression::OTHERWISE_RULE_ID`）。
    pub rule_id: Option<String>,
    /// 目标翻译结果（顺序 = `then` 的 JSON 数组顺序，决定 `finalize()` 的去重优先级）。
    pub targets: Vec<ResolvedTarget>,
    /// 求值痕迹（`命中 r_big` / `规则 r1 求值失败（Missing: …）`）。
    pub trace: Vec<String>,
}

/// 节点是否走「动态表达式」来源。
pub fn is_expression(node: &SnapshotNode) -> bool {
    node.assignee_type
        .as_deref()
        .map(|raw| {
            AssigneeKind::parse(raw)
                .map(|kind| kind == AssigneeKind::Expression)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// 表达式审批人：把节点配置求值成「一组目标」（**纯函数**：不查库、不 `await`）。
///
/// # 三条出错语义（EXPRESSION-DSL §3.6 第 8/9 行、§5.5 第 ③ 层、§8.6）
///
/// | 情形 | 返回 | 为什么 |
/// |---|---|---|
/// | 引用了 `PREV_APPROVER` | `400 + 65033` | 保留词：发布期已拦，这里是「库被直接改过」的最后一道，透传而不是静默 |
/// | 结构非法 / `config` 里没有 `expression` / `version` 不认识 | `501 + 65032` | S4 失败不静默：绝不退化成「空审批人 → 节点自动通过」 |
/// | `when` 求值出错（`Missing` / `TypeMismatch`） | **不算错**：按「不成立」继续下一条，记 trace | D-E7 |
///
/// # 为什么返回 `Result`（而 `expression::eval_expression` 不返回）
///
/// 求值本身确实「没有错误通道」（命中 / 不命中 / 走兜底），但**解析**有：
/// 一段坏 JSON / 一个保留词，必须让调用方拿到明确的 HTTP 码，而不是「0 个目标 → 自动通过」。
pub fn expression_plans(
    config: &Json,
    eval: &EvalContext,
    ctx: &ResolveContext,
) -> std::result::Result<ExpressionPlans, ApiError> {
    if let Some(value) = config.get("expression")
        && graph::expression_uses_prev_approver(value)
    {
        return Err(ApiError::new(
            400,
            error_code::FLOW_EXPRESSION_INVALID,
            "审批人表达式引用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源",
        ));
    }

    let parsed = expression::parse_config(config).map_err(|reason| {
        ApiError::new(
            501,
            error_code::FLOW_EXPRESSION_UNSUPPORTED,
            format!("审批人来源 EXPRESSION 的表达式无法识别（可能是旧版本数据）：{reason}"),
        )
    })?;

    let mut trace: Vec<String> = Vec::new();
    let outcome = expression::eval_expression_with_trace(&parsed, eval, &mut trace);
    let rule_id = outcome.rule_id().map(str::to_string);
    let resolved = expression::resolve_targets(outcome.targets(), ctx);
    trace.extend(resolved.notes.iter().cloned());

    Ok(ExpressionPlans {
        rule_id,
        targets: resolved.targets,
        trace,
    })
}

// ==================== DB 解析 ====================

/// 解析一个节点的审批人（**在事务内调用**，`ex` 传 `&tx`）。
///
/// 返回 [`Resolution::finalized`]：调用方照它落 `biz_flow_task` 行；
/// `error` 非空时调用方应让整单回滚（`FALLBACK_ADMIN` 除外，见下）。
///
/// # 入参 `eval`
///
/// `EXPRESSION` 节点的 `when` 子树与**条件边**共用同一个求值上下文（`form` / `initiator` /
/// `instance` / `task`）。调用方传引擎已构造好的 [`EvalContext`]（`engine::eval_context()`），
/// 不在这里二次构造：那会与 F1 修好的 11 个白名单字段各写一份，两份迟早漂移。
///
/// # 异常情况
///
/// - `400 + 65033`：表达式引用了 `PREV_APPROVER`（保留词，见 [`expression_plans`]）；
/// - `501 + 65032`：快照里 `assignee_type=EXPRESSION` 但表达式缺失 / 结构坏 / `version` 不认识；
/// - 数据库异常 → [`DbError`]。
pub async fn resolve(
    ex: &impl SqlExecutor,
    node: &SnapshotNode,
    ctx: &InstanceCtx,
    eval: &EvalContext,
) -> DbResult<Resolution> {
    let mut trace: Vec<String> = Vec::new();
    let mut rule_id: Option<String> = None;

    // ① 先得到「要查哪些来源」：静态来源 = 1 条计划；EXPRESSION = 规则求值后的一组计划
    //    （多个 target = **并集**，不是会签/或签 —— 会签/或签是节点级 `approval_mode`）。
    let plans: Vec<SourcePlan> = if is_expression(node) {
        let resolve_ctx = ResolveContext {
            initiator_dept_id: ctx.initiator_dept_id,
        };
        let plans = match expression_plans(&node.config, eval, &resolve_ctx) {
            Ok(plans) => plans,
            Err(error) => {
                return Ok(Resolution {
                    finalized: Finalized::failed(
                        error,
                        vec![format!("节点 {} 的审批人表达式无法解析", node.key)],
                    ),
                    trace,
                });
            }
        };
        trace.extend(plans.trace);
        rule_id = plans.rule_id;
        let mut out: Vec<SourcePlan> = Vec::with_capacity(plans.targets.len());
        for target in plans.targets {
            match target {
                ResolvedTarget::Plan(plan) => out.push(plan),
                // ROLE 只给了 role_code（跨环境稳定），code→id 的查询在这里做（§8.3 方案 B）
                ResolvedTarget::RoleCodes(codes) => {
                    let ids = load_role_ids_by_code(ex, &codes, &mut trace).await?;
                    out.push(SourcePlan::Roles(ids));
                }
            }
        }
        trace.push(format!("EXPRESSION 展开成 {} 个来源计划", out.len()));
        out
    } else {
        vec![plan_of(node)]
    };

    // ② 逐一查人（三层去重的第 1 层在下一步 `finalize()` 里，逐字未改）
    let mut candidates: Vec<Candidate> = Vec::new();
    for plan in &plans {
        if let SourcePlan::Unsupported(raw) = plan {
            return Ok(Resolution {
                finalized: Finalized {
                    error: Some(ApiError::new(
                        501,
                        error_code::FLOW_EXPRESSION_UNSUPPORTED,
                        format!("审批人来源 {raw} 不支持（未配置 / 取值不认识）"),
                    )),
                    notes: vec![format!("节点 {} 的 assignee_type={raw} 不支持", node.key)],
                    ..Finalized::default()
                },
                trace,
            });
        }
        candidates.extend(load_plan(ex, plan, ctx, &mut trace).await?);
    }

    // ③ 名字快照：没查到用户（软删）的人不会出现在候选里，这里只对「查到了」的补 trace
    trace.push(format!("候选 {} 人（去重前）", candidates.len()));
    let self_policy = SelfPolicy::parse(&node.self_approval);
    let empty_policy = EmptyPolicy::parse(&node.empty_assignee);
    let mut notes: Vec<String> = Vec::new();
    let mut finalized = match filter_candidates(
        candidates.clone(),
        self_policy,
        node.allow_robot,
        ctx.initiator_id,
        &mut notes,
    ) {
        Ok(filtered) => Finalized {
            approvers: filtered.approvers,
            skipped: filtered.skipped,
            notes,
            ..Finalized::default()
        },
        // 65019（FORBID 命中发起人）：整单失败，不再升级 —— 财务 / 法务节点的语义就是
        // 「宁可卡住也不能换个人稀里糊涂签掉」。
        Err(error) => {
            return Ok(Resolution {
                finalized: Finalized::failed(error, notes),
                trace,
            });
        }
    };
    finalized.rule_id = rule_id;

    // ④ 升级链（P3-02）：**只在「有角色来源且被剔光」时才走**
    //
    //   · `SKIP_SELF_APPROVAL`：自审策略把审批人全剔了（`skipped` 非空）——
    //     ARCH-8 场景 B「角色只有制单人一人」；
    //   · `NO_MEMBER`：角色一个成员都没有 —— 同场景的另一形态。
    //
    // 其它来源（USER / 部门负责人 / 发起人主管 / 表单变量）解析出 0 人不是「升级链」
    // 能解决的问题（链是角色→角色），照旧走 `empty_assignee`。
    if finalized.approvers.is_empty() {
        let start_roles = escalation_start_roles(&plans);
        let reason = if !finalized.skipped.is_empty() {
            Some(ESCALATE_REASON_SKIP)
        } else if candidates.is_empty() && !start_roles.is_empty() {
            Some(ESCALATE_REASON_NO_MEMBER)
        } else {
            None
        };
        if let (Some(reason), false) = (reason, start_roles.is_empty()) {
            match escalate(
                ex,
                &start_roles,
                reason,
                node.allow_robot,
                ctx.initiator_id,
                &mut trace,
            )
            .await?
            {
                EscalationOutcome::Applied(escalation, approvers) => {
                    trace.push(format!(
                        "升级链接管：原角色 {}（{}）→ 上级「{}」{} 人",
                        escalation.from_role_code,
                        escalation.reason_label(),
                        escalation
                            .levels
                            .last()
                            .map(|level| level.role_name.clone())
                            .unwrap_or_default(),
                        approvers.len()
                    ));
                    finalized.notes.push(format!(
                        "本节点审批人被自审策略剔光，已沿升级链改派：{}",
                        escalation.chain_text()
                    ));
                    finalized.escalated_from_role = Some(escalation.from_role_code.clone());
                    finalized.escalate_reason = Some(escalation.reason.clone());
                    finalized.approvers = approvers;
                    finalized.auto_pass = false;
                    finalized.fallback_admin = false;
                    finalized.error = None;
                    finalized.escalation = Some(escalation);
                    finalized.assigned_by_rule = Some(ASSIGNED_BY_ESCALATION.to_string());
                }
                EscalationOutcome::Exhausted(escalation) => {
                    // 到顶仍无人：**不在这里报 422**（交给 empty_assignee / 引擎统一处置），
                    // 但把链路带出去 —— 报文与结构化诊断都要能回答「找过谁、各有几个人」。
                    finalized.notes.push(format!(
                        "升级链已走完，仍无可用审批人：{}",
                        escalation.chain_text()
                    ));
                    finalized.escalation = Some(escalation);
                }
                EscalationOutcome::Invalid(error) => {
                    // 65040 / 65041：配置非法，整单失败（静默跳过会把配错变成事故）
                    return Ok(Resolution {
                        finalized: Finalized::failed(error, finalized.notes),
                        trace,
                    });
                }
            }
        }
    }

    // ⑤ 兜底人（P3-03 / ARCH-8 第 ③ 层）：**升级链之后**、空审批人策略之前。
    //
    //   到这里 `approvers` 仍为空 = 「升级链走完也没人」（或该节点压根没有升级链可走）。
    //   节点配了 fallback_user / fallback_role 就由它接管；两列都空则保持原行为。
    if finalized.approvers.is_empty() {
        match apply_fallback(ex, node, node.allow_robot, ctx.initiator_id, &mut trace).await? {
            FallbackOutcome::NotConfigured => {
                // 只留痕；报文由下面的 `dispatch_hint` 统一拼（「升级链与兜底人均未配置」）
                finalized.notes.push(
                    "本节点没有兜底人配置（fallback_user_id / fallback_role_id 均为空）"
                        .to_string(),
                );
            }
            FallbackOutcome::Applied(fallback, approvers) => {
                trace.push(format!(
                    "兜底人接管：{}（{}）→ {} 人",
                    fallback.target_label(),
                    fallback.reason,
                    approvers.len()
                ));
                finalized.notes.push(format!(
                    "升级链走完仍无人，已由兜底人「{}」接管",
                    fallback.target
                ));
                finalized.approvers = approvers;
                finalized.auto_pass = false;
                finalized.fallback_admin = false;
                finalized.error = None;
                finalized.assigned_by_rule = Some(ASSIGNED_BY_FALLBACK.to_string());
                finalized.fallback = Some(fallback);
            }
            FallbackOutcome::Invalid(error) => {
                // 65042：兜底配错必须吵（静默当成「没配」会让运维以为已经兜住了）
                return Ok(Resolution {
                    finalized: Finalized::failed(error, finalized.notes),
                    trace,
                });
            }
            FallbackOutcome::SelfConflict(error, fallback) => {
                // 兜底人就是发起人：内控不允许自审 —— 不放行、也不再往下走 empty_assignee
                finalized.fallback = Some(fallback);
                finalized
                    .notes
                    .push("兜底人即发起人，内控不允许自审（兜底不是免审）".to_string());
                return Ok(Resolution {
                    finalized: Finalized::failed(error, finalized.notes),
                    trace,
                });
            }
        }
    }

    // ⑥ 空审批人策略（升级链 / 兜底人都不适用时才走这里）
    apply_empty_policy(&mut finalized, empty_policy);

    // `empty_assignee=ERROR`：把「本节点没有一个可用审批人」补成**可执行**的配置指引。
    //
    // P3-03：报文必须能回答「降级链走到哪了」（升级链走到几级、兜底人配没配），
    // 所以先算好 `dispatch` 再借 `error`（同时借用 `finalized` 的不可变字段会冲突）。
    let dispatch = dispatch_hint(finalized.escalation.as_ref(), finalized.fallback.as_ref());
    if let Some(error) = finalized.error.as_mut()
        && error.code == error_code::FLOW_NO_ASSIGNEE
        && error.msg.contains("empty_assignee=ERROR")
    {
        error.msg = format!(
            "节点「{}」没有任何可用审批人（来源 {} 解析出 0 人；{dispatch}）。\
             该节点配置了 empty_assignee=ERROR，不会兜底给管理员。请依次检查：\
             ①「流程定义 → 本流程 → 节点「{}」」的审批人是否指向有在职成员的角色 / 人；\
             ②「系统管理 → 角色管理 → 上级角色」把升级链上每一级都指派在职成员；\
             ③给节点配「兜底审批人」或「兜底角色」（升级链走完仍无人时由它接管，\
             要多人兜底就配角色）。若该岗位确实无人，可改由有审批权的人提交本单。",
            node.name,
            node.assignee_type.as_deref().unwrap_or("未配置"),
            node.name
        );
    }
    trace.push(format!(
        "去重 + 策略后：待办 {} 行 / SKIPPED 留痕 {} 行 / 自动通过={} / 升级={}",
        finalized.approvers.len(),
        finalized.skipped.len(),
        finalized.auto_pass,
        finalized.escalation.is_some()
    ));

    // FALLBACK_ADMIN：回退给平台超管（这里只查一次，不再走策略 —— 超管兜底不该再被过滤）
    if finalized.fallback_admin {
        let admins = load_admins(ex).await?;
        trace.push(format!("FALLBACK_ADMIN 命中 {} 个超管", admins.len()));
        if admins.is_empty() {
            finalized.error = Some(ApiError::new(
                422,
                error_code::FLOW_NO_ASSIGNEE,
                "本节点没有有效审批人，且平台没有可兜底的超级管理员",
            ));
        } else {
            finalized.approvers = admins;
            finalized.fallback_admin = false;
            finalized.auto_pass = false;
        }
    }

    // ★ FIX-1（新环境开箱即用）：`65018` 带上**结构化诊断**
    //
    // 老报文只有一句话，前端没法做引导（新库「只有 admin 一个账号」时尤甚：
    // 用户只知道「没有审批人」，不知道缺哪个角色、那个角色几个人、下一步点哪里）。
    // 这里统一挂一份载荷 —— 覆盖 `empty_assignee=ERROR` 与「连超管都没有」两条路径；
    // 引擎还会在报错前用 `no_assignee::set_node_id` 补上实例节点行 id。
    //
    // 载荷组装失败**不影响**错误本身（`payload()` 内部查库失败会退化成不带角色字段的载荷）。
    if let Some(error) = finalized.error.as_mut()
        && error.code == error_code::FLOW_NO_ASSIGNEE
    {
        let signal = no_assignee::Signal::of(node, &candidates, ctx.initiator_id);
        let payload = no_assignee::payload(
            ex,
            ctx.instance_id,
            node,
            signal,
            Vec::new(),
            finalized.escalation.as_ref(),
        )
        .await;
        no_assignee::attach(error, payload);
    }

    Ok(Resolution { finalized, trace })
}

/// 执行一条来源计划（**逐字搬运** D2 的 9 个分支，逻辑零改动）。
///
/// 抽出来只为让 `resolve()` 能跑「一组计划」（`EXPRESSION` 的多个 target），
/// 单来源节点的行为与抽之前**完全一致**：`Unsupported` 由调用方提前拦截。
async fn load_plan(
    ex: &impl SqlExecutor,
    plan: &SourcePlan,
    ctx: &InstanceCtx,
    trace: &mut Vec<String>,
) -> DbResult<Vec<Candidate>> {
    let candidates = match plan {
        SourcePlan::Users(ids) => {
            let candidates = load_users(ex, ids, AssigneeKind::User).await?;
            trace.push(format!(
                "USER 命中 {} 行（配置 {} 个）",
                candidates.len(),
                ids.len()
            ));
            candidates
        }
        SourcePlan::Roles(role_ids) => {
            let candidates = load_role_members(ex, role_ids).await?;
            trace.push(format!(
                "ROLE 展开 {} 行（角色 {} 个）",
                candidates.len(),
                role_ids.len()
            ));
            candidates
        }
        SourcePlan::Initiator => {
            let candidates = load_users(ex, &[ctx.initiator_id], AssigneeKind::Initiator).await?;
            trace.push(format!("INITIATOR 命中 {} 行", candidates.len()));
            candidates
        }
        SourcePlan::InitiatorSuperior(levels) => load_superior(ex, ctx, *levels, trace).await?,
        SourcePlan::DeptLeaders(dept_ids) => {
            let dept_ids = if dept_ids.is_empty() {
                ctx.initiator_dept_id.into_iter().collect::<Vec<_>>()
            } else {
                dept_ids.clone()
            };
            let candidates = load_dept_leaders(ex, &dept_ids).await?;
            trace.push(format!(
                "DEPT_LEADER 命中 {} 行（部门 {} 个）",
                candidates.len(),
                dept_ids.len()
            ));
            candidates
        }
        SourcePlan::TaskParty(party) => {
            let ids = task_party_ids(ex, ctx, *party).await?;
            let candidates = load_users(ex, &ids, task_source(*party)).await?;
            trace.push(format!(
                "{} 命中 {} 行（biz_type={} / biz_id={}）",
                task_source(*party).as_str(),
                candidates.len(),
                ctx.biz_type,
                ctx.biz_id.map(|id| id.to_string()).unwrap_or_default()
            ));
            candidates
        }
        SourcePlan::FormVar(key) => {
            let ids = form_var_ids(&ctx.form_data, key);
            let candidates = load_users(ex, &ids, AssigneeKind::FormVar).await?;
            trace.push(format!("FORM_VAR({key}) 命中 {} 行", candidates.len()));
            candidates
        }
        SourcePlan::Admins => {
            let candidates = load_admins(ex).await?;
            trace.push(format!("ADMIN 命中 {} 行", candidates.len()));
            candidates
        }
        // 调用方在循环里提前 `return`，这里只兜底（不查库、不记 trace）
        SourcePlan::Unsupported(_) => Vec::new(),
    };
    Ok(candidates)
}

/// 角色编码 → 角色 id（`EXPRESSION` 的 `ROLE` 目标专用，§8.3 方案 B）。
///
/// # 两条必须做对的事
///
/// 1. **必须带 `deleted_at IS NULL`**：`sys_role.role_code` 的唯一性是**部分唯一索引**
///    （`WHERE deleted_at IS NULL`），不带这个谓词可能命中已删的同名角色；
/// 2. **查不到不算错**：code 不存在 = 运维态（角色被删/改名），这一条 target 解析出 0 人 →
///    走 `empty_assignee` 策略并记 trace，**不是** 501（那是「代码不认识的数据」才有的待遇）。
///
/// `pub(crate)`：预演接口（`POST /flow/node/task/preview`）要按 `role_codes` 算「谁来干」，
/// 与表达式审批人走同一个 code→id 解析，不许各写一份（否则「预演算 3 人、真跑算 0 人」）。
pub(crate) async fn load_role_ids_by_code(
    ex: &impl SqlExecutor,
    codes: &[String],
    trace: &mut Vec<String>,
) -> DbResult<Vec<Uuid>> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let body = codes.join(",");
    let rows = ex
        .query_sql(
            "SELECT id, role_code FROM sys_role \
              WHERE role_code = ANY(?::text::text[]) AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(format!("{{{body}}}"))],
        )
        .await?;

    let mut found: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    for row in &rows {
        let code: String = row.get_typed("role_code")?;
        let id: Uuid = row.get_typed("id")?;
        found.insert(code, id);
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(codes.len());
    let mut missing: Vec<&str> = Vec::new();
    for code in codes {
        match found.get(code) {
            Some(id) => {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
            None => missing.push(code.as_str()),
        }
    }
    trace.push(format!(
        "ROLE code→id：{} 个编码命中 {} 个角色（请求 {} 个）",
        codes.len() - missing.len(),
        ids.len(),
        codes.len()
    ));
    if !missing.is_empty() {
        trace.push(format!(
            "角色编码不存在（已被删除或改名）：{} → 该来源解析 0 人",
            missing.join("、")
        ));
    }
    Ok(ids)
}

fn task_source(party: TaskParty) -> AssigneeKind {
    match party {
        TaskParty::Owner => AssigneeKind::TaskOwner,
        TaskParty::Creator => AssigneeKind::TaskCreator,
        TaskParty::Assigner => AssigneeKind::TaskAssigner,
    }
}

// SCOPE-EXEMPT: 审批人解析按**节点配置里的显式 id** 取人（`node_assignee` 的
// SCOPE-EXEMPT: user/role/dept id、实例发起人、`biz_task` 关联单据的负责人/创建人/分派人、
// SCOPE-EXEMPT: `role_code = SUPER_ADMIN` 的兜底管理员）。这不是「列表查询」，而是一次授权判定：
// SCOPE-EXEMPT: 「这批人该不该审这张单」由流程配置决定，与**查看者**的数据范围无关
// SCOPE-EXEMPT: —— 把部门范围注入进来会让「隔级主管该审」变成「隔级主管看不到这张单」。
// SCOPE-EXEMPT: 详见 `docs/批次D-工作流审批-技术架构.md` §4.3（8 种来源）与 §9.1（实例可见性另算）。

/// 按 id 批量取用户（软删不返回；停用返回但 `active = false`，由 [`finalize`] 剔除并留痕）。
async fn load_users(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
    source: AssigneeKind,
) -> DbResult<Vec<Candidate>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            "SELECT u.id AS user_id, \
                    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, \
                    u.user_type AS user_type, (u.status = 1) AS active \
               FROM sys_user u \
              WHERE u.id = ANY(?::text::uuid[]) AND u.deleted_at IS NULL"
                .to_string(),
            vec![array_param(ids)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let user_id: Uuid = row.get_typed("user_id")?;
        let user_type: String = row.get_typed("user_type")?;
        out.push(Candidate {
            user_id,
            name: row.get_typed("name")?,
            source,
            source_ref: None,
            is_robot: user_type.eq_ignore_ascii_case("ROBOT"),
            active: row.get_typed("active")?,
        });
    }
    out.sort_by_key(|candidate| candidate.user_id);
    Ok(out)
}

/// `ROLE` 展开：`sys_user_role` ∩ 未过期 ∩ 未删用户。
async fn load_role_members(ex: &impl SqlExecutor, role_ids: &[Uuid]) -> DbResult<Vec<Candidate>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            "SELECT ur.user_id AS user_id, ur.role_id AS ref_id, \
                    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, \
                    u.user_type AS user_type, (u.status = 1) AS active \
               FROM sys_user_role ur \
               JOIN sys_user u ON u.id = ur.user_id \
              WHERE ur.role_id = ANY(?::text::uuid[]) \
                AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
                AND u.deleted_at IS NULL"
                .to_string(),
            vec![array_param(role_ids)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let user_type: String = row.get_typed("user_type")?;
        out.push(Candidate {
            user_id: row.get_typed("user_id")?,
            name: row.get_typed("name")?,
            source: AssigneeKind::Role,
            source_ref: row.get_typed("ref_id")?,
            is_robot: user_type.eq_ignore_ascii_case("ROBOT"),
            active: row.get_typed("active")?,
        });
    }
    out.sort_by_key(|candidate| (candidate.user_id, candidate.source_ref));
    Ok(out)
}

// ==================== 升级链（P3-02 · ARCH-8 第 ② 层） ====================
//
// 场景：`self_approval=SKIP` 把发起人本人剔光（或角色本来就 0 成员）⇒ 节点 0 人。
// P3-02 之前这里直接 422 / `65018`（「角色里只有制单人」的小公司永远发不出单）。
// 现在沿 `sys_role.escalation_role_id` **逐级上溯**，找到第一个有可用成员的角色。
//
// 三条纪律（决策 D5 / 卡面 §2）：
//
// 1. **链条来自库，不来自代码**：本文件只认 `escalation_role_id` 这个指针，
//    不认识任何具体角色编码（`SALES_CLERK → SALES_SUPERVISOR` 只出现在迁移与管理员界面）；
// 2. **每一级都重跑过滤**：停用 / ROBOT / 自审（`filter_candidates`），
//    「上一级的成员恰好也是发起人」不会被塞回待办 —— 否则只是把自审挪了个位置；
// 3. **配错要吵**：自引用 / 成环 / 指向已删角色 → `65040`；超过 8 级 → `65041`。
//    静默跳过会让「以为有上级兜底」变成事故。

/// 升级链最多上溯几级；再深就是配置错误（`65041`）。
pub const ESCALATION_MAX_LEVELS: usize = 8;

/// 升级原因（写 `biz_flow_task.escalate_reason`）：自审策略把审批人剔光。
pub const ESCALATE_REASON_SKIP: &str = "SKIP_SELF_APPROVAL";

/// 升级原因：角色里一个成员都没有。
pub const ESCALATE_REASON_NO_MEMBER: &str = "NO_MEMBER";

/// `biz_flow_task.escalated_from_role` 的列宽（`varchar(40)`）。
const ESCALATED_FROM_ROLE_LIMIT: usize = 40;

/// 升级链上的一个角色（`sys_role` 的标识列 + 上级指针）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChainNode {
    /// 角色 id。
    pub id: Uuid,
    /// 角色编码（写 `escalated_from_role` / 报文）。
    pub role_code: String,
    /// 角色名称（报文 / 前端提示）。
    pub role_name: String,
    /// 上级角色（`sys_role.escalation_role_id`；`None` = 链到此为止）。
    pub parent: Option<Uuid>,
}

/// 升级链走过的一级（诊断载荷 + 日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationLevel {
    /// 角色 id。
    pub role_id: Uuid,
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 该角色下的**在位成员行数**（过滤前）。
    pub member_rows: usize,
    /// 过滤（停用 / ROBOT / 自审）之后还能签的人数。
    pub usable: usize,
}

/// 一次升级尝试的完整痕迹。
#[derive(Debug, Clone, Default)]
pub struct Escalation {
    /// 升级原因（[`ESCALATE_REASON_SKIP`] / [`ESCALATE_REASON_NO_MEMBER`]）。
    pub reason: String,
    /// **原本该审**的角色编码（多个起始角色用 `,` 连接，已按列宽截断）。
    pub from_role_code: String,
    /// 原本该审的角色名（报文用）。
    pub from_role_name: String,
    /// 走过的每一级（含最终命中的那一级）。
    pub levels: Vec<EscalationLevel>,
    /// 是否在某一级找到了可用审批人。
    pub applied: bool,
}

impl Escalation {
    /// 链路的人读形式：`销售员(0/1) → 销售主管(0/0) → 销售总监(2/2)`。
    ///
    /// 括号里是 `可用/成员`：**成员 0 行**与「有成员但全被过滤」是两种不同的处置
    /// （前者去加人，后者先查人是不是停用了）。
    pub fn chain_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.from_role_code.is_empty() {
            parts.push(format!(
                "{}（原角色，{}）",
                if self.from_role_name.is_empty() {
                    self.from_role_code.clone()
                } else {
                    self.from_role_name.clone()
                },
                self.reason_label()
            ));
        }
        for level in &self.levels {
            parts.push(format!(
                "{}（{}/{} 人）",
                level.role_name, level.usable, level.member_rows
            ));
        }
        parts.join(" → ")
    }

    /// 原因的人读标签（报文里用）。
    pub fn reason_label(&self) -> &'static str {
        match self.reason.as_str() {
            ESCALATE_REASON_SKIP => "自审跳过",
            ESCALATE_REASON_NO_MEMBER => "角色无成员",
            _ => "无可用审批人",
        }
    }

    /// 结构化诊断里的链路（`no_assignee` 载荷用）。
    pub fn to_payload_chain(&self) -> Vec<Json> {
        self.levels
            .iter()
            .map(|level| {
                serde_json::json!({
                    "role_id": level.role_id.to_string(),
                    "role_code": level.role_code,
                    "role_name": level.role_name,
                    "member_count": level.member_rows,
                    "usable_count": level.usable,
                })
            })
            .collect()
    }
}

/// 升级尝试的三种结局。
#[derive(Debug)]
pub enum EscalationOutcome {
    /// 在某一级找到了可用成员（附「他们是谁」）。
    Applied(Escalation, Vec<Candidate>),
    /// 走到顶也没有人（交给兜底人 P3-03 / `empty_assignee`）。
    Exhausted(Escalation),
    /// 升级链配置非法（`65040` 自引用 / 环 / 角色不存在，`65041` 超过 8 级）。
    Invalid(ApiError),
}

/// 查一个角色（未软删）的 id / code / name / 上级；查不到返回 `None`。
async fn load_role_chain_node(
    ex: &impl SqlExecutor,
    role_id: Uuid,
) -> DbResult<Option<RoleChainNode>> {
    let rows = ex
        .query_sql(
            "SELECT id, role_code, role_name, escalation_role_id \
               FROM sys_role WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&role_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(RoleChainNode {
        id: row.get_typed("id")?,
        role_code: row.get_typed("role_code")?,
        role_name: row.get_typed("role_name")?,
        parent: row.get_typed("escalation_role_id")?,
    }))
}

/// 把起始角色编码拼成 `escalated_from_role` 列值（`varchar(40)`，超出按字符截断）。
fn join_role_codes(nodes: &[RoleChainNode]) -> String {
    let joined = nodes
        .iter()
        .map(|node| node.role_code.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if joined.chars().count() <= ESCALATED_FROM_ROLE_LIMIT {
        joined
    } else {
        joined.chars().take(ESCALATED_FROM_ROLE_LIMIT).collect()
    }
}

/// 沿 `sys_role.escalation_role_id` 逐级上溯找可用审批人（P3-02 / ARCH-8 场景 B）。
///
/// # 参数
///
/// - `start_role_ids`：节点配置的（或被 EXPRESSION 解析出的）角色 id —— 升级链的**起点**；
/// - `reason`：为什么走到这一步（[`ESCALATE_REASON_SKIP`] / [`ESCALATE_REASON_NO_MEMBER`]），
///   原样写进待办留痕；
/// - `allow_robot`：节点的 ROBOT 开关（每一级都按同一口径过滤）；
/// - `initiator`：发起人 —— 每一级都要再跑一次自审过滤（**不许把发起人从上一级塞回来**）；
/// - `trace`：解析痕迹（`Resolution.trace` / 日志）。
///
/// # 规则
///
/// - 起点角色**已删**：跳过（那是 `ROLE_NOT_FOUND`，不是链配错）；
/// - 链上某级角色**已删 / 不存在**：`65040`（`escalation_role_id` 指向了不存在的角色）；
/// - **成环**（`visited` 命中）：`65040`；
/// - **超过 [`ESCALATION_MAX_LEVELS`] 级**：`65041`；
/// - 每一级取「该角色在位成员」→ 过 `filter_candidates`（停用 / ROBOT / 自审）→ 有人即命中；
/// - 到顶仍无人：`Exhausted`（**不在本卡兜底**，交 P3-03；报文由引擎带上链路）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；配置非法 → [`EscalationOutcome::Invalid`]（不是 `DbError`：
/// 那是**用户配置**问题，要回给前端，不能记成 500）。
pub async fn escalate(
    ex: &impl SqlExecutor,
    start_role_ids: &[Uuid],
    reason: &str,
    allow_robot: bool,
    initiator: Uuid,
    trace: &mut Vec<String>,
) -> DbResult<EscalationOutcome> {
    let mut from_nodes: Vec<RoleChainNode> = Vec::new();
    for role_id in start_role_ids {
        match load_role_chain_node(ex, *role_id).await? {
            Some(node) => from_nodes.push(node),
            None => trace.push(format!("升级链起点角色 {role_id} 已删除，跳过")),
        }
    }
    let from_role_code = join_role_codes(&from_nodes);
    let from_role_name = from_nodes
        .iter()
        .map(|node| node.role_name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let mut escalation = Escalation {
        reason: reason.to_string(),
        from_role_code,
        from_role_name,
        levels: Vec::new(),
        applied: false,
    };
    trace.push(format!(
        "升级链启动（{}）：起点 {} 个角色 {}",
        escalation.reason_label(),
        from_nodes.len(),
        if escalation.from_role_code.is_empty() {
            "（无）".to_string()
        } else {
            escalation.from_role_code.clone()
        }
    ));

    let mut visited: BTreeSet<Uuid> = from_nodes.iter().map(|node| node.id).collect();
    let mut frontier: Vec<Uuid> = Vec::new();
    for node in &from_nodes {
        if let Some(parent) = node.parent
            && !frontier.contains(&parent)
        {
            frontier.push(parent);
        }
    }

    for depth in 0..ESCALATION_MAX_LEVELS {
        if frontier.is_empty() {
            break;
        }
        // 本级：先查环（visited），再把 frontier 里的角色读出来（缺失 → 65040）
        if let Some(repeated) = register_frontier(&frontier, &mut visited) {
            return Ok(EscalationOutcome::Invalid(ApiError::new(
                422,
                error_code::FLOW_ESCALATION_INVALID,
                format!(
                    "角色升级链成环：角色 {repeated} 在同一链条上出现了两次\
                     （请到「系统管理 → 角色管理 → 上级角色」断开环）"
                ),
            )));
        }
        let mut nodes: Vec<RoleChainNode> = Vec::with_capacity(frontier.len());
        for role_id in &frontier {
            match load_role_chain_node(ex, *role_id).await? {
                Some(node) => nodes.push(node),
                None => {
                    return Ok(EscalationOutcome::Invalid(ApiError::new(
                        422,
                        error_code::FLOW_ESCALATION_INVALID,
                        format!(
                            "角色升级链配置非法：escalation_role_id 指向的角色 {role_id} 不存在\
                             （已被删除），请到「系统管理 → 角色管理 → 上级角色」重新选择"
                        ),
                    )));
                }
            }
        }
        nodes.sort_by(|left, right| left.role_code.cmp(&right.role_code));

        let mut level_members: Vec<(Uuid, Vec<Candidate>)> = Vec::with_capacity(nodes.len());
        for node in &nodes {
            level_members.push((node.id, load_role_members(ex, &[node.id]).await?));
        }
        let (level_rows, hit) =
            decide_level(&nodes, &level_members, allow_robot, initiator, depth, trace);
        escalation.levels.extend(level_rows);
        if let Some(approvers) = hit {
            escalation.applied = true;
            trace.push(format!(
                "升级链命中：{}（共 {} 人）",
                escalation.chain_text(),
                approvers.len()
            ));
            return Ok(EscalationOutcome::Applied(escalation, approvers));
        }
        // 下一级 = 本级所有角色的上级（去重，保持顺序）
        frontier = next_frontier(&nodes);
    }

    if !frontier.is_empty() {
        return Ok(EscalationOutcome::Invalid(ApiError::new(
            422,
            error_code::FLOW_ESCALATION_TOO_DEEP,
            format!(
                "角色升级链超过 {ESCALATION_MAX_LEVELS} 级仍未找到上级审批人\
                 （已走过：{}）。请到「系统管理 → 角色管理 → 上级角色」检查链条是否配反了",
                escalation.chain_text()
            ),
        )));
    }
    trace.push(format!(
        "升级链已走完，仍无可用审批人：{}",
        escalation.chain_text()
    ));
    Ok(EscalationOutcome::Exhausted(escalation))
}

/// 处理升级链的**一级**（纯函数，可单测）。
///
/// 每一级都要重跑 `filter_candidates`（停用 / ROBOT / 自审）：升级链不是「换个角色就完事」，
/// 上级角色里也可能只有发起人一个人（很多小公司总经理就是老板本人）。
///
/// # 参数
///
/// - `nodes`：本级角色（调用方已按 `role_code` 排序，保证结果稳定）；
/// - `members`：与 `nodes` 一一对应的成员表（`(role_id, 候选)`）；
/// - `depth`：层级序号（0 起，只用于留痕文案）。
///
/// # 返回值
///
/// `(本级留痕, 命中的可用成员)`；`None` = 本级一个能签的人都没有（继续上溯）。
fn decide_level(
    nodes: &[RoleChainNode],
    members: &[(Uuid, Vec<Candidate>)],
    allow_robot: bool,
    initiator: Uuid,
    depth: usize,
    trace: &mut Vec<String>,
) -> (Vec<EscalationLevel>, Option<Vec<Candidate>>) {
    let mut rows: Vec<EscalationLevel> = Vec::with_capacity(nodes.len());
    let mut hit: Option<Vec<Candidate>> = None;
    for node in nodes {
        let candidates = members
            .iter()
            .find(|(role_id, _)| *role_id == node.id)
            .map(|(_, candidates)| candidates.clone())
            .unwrap_or_default();
        let member_rows = candidates.len();
        let mut notes: Vec<String> = Vec::new();
        // 升级后的每一级一律按 SKIP 处理自审：发起人就是不能自己审，与节点上的
        // `self_approval` 无关（FORBID 在 filter 阶段就已经整单失败了，走不到这里）。
        let usable = match filter_candidates(
            candidates,
            SelfPolicy::Skip,
            allow_robot,
            initiator,
            &mut notes,
        ) {
            Ok(filtered) => filtered.approvers,
            // Skip 策略不会产生 65019；真出现了说明策略被改坏了，按 0 人处理并留痕
            Err(_) => Vec::new(),
        };
        trace.push(format!(
            "升级链第 {} 级：{}（{}）成员 {} 人 / 过滤后可用 {} 人{}",
            depth + 1,
            node.role_name,
            node.role_code,
            member_rows,
            usable.len(),
            if notes.is_empty() {
                String::new()
            } else {
                format!("（{}）", notes.join("；"))
            }
        ));
        rows.push(EscalationLevel {
            role_id: node.id,
            role_code: node.role_code.clone(),
            role_name: node.role_name.clone(),
            member_rows,
            usable: usable.len(),
        });
        if hit.is_none() && !usable.is_empty() {
            hit = Some(usable);
        }
    }
    (rows, hit)
}

/// 把本级 frontier 登记进 `visited`；返回**第一个重复出现**的角色 id（= 成环）。
///
/// 纯函数：单测直接构造 frontier 序列就能钉住 `65040` 的判据，
/// 不必为了「造一个环」去改库。
fn register_frontier(frontier: &[Uuid], visited: &mut BTreeSet<Uuid>) -> Option<Uuid> {
    for role_id in frontier {
        if !visited.insert(*role_id) {
            return Some(*role_id);
        }
    }
    None
}

/// 下一级要查的角色 id：本级所有角色的上级（去重、保持顺序，纯函数）。
fn next_frontier(nodes: &[RoleChainNode]) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = Vec::new();
    for node in nodes {
        if let Some(parent) = node.parent
            && !out.contains(&parent)
        {
            out.push(parent);
        }
    }
    out
}

/// 解析计划里的角色 id（升级链的起点；`EXPRESSION` 展开后的 ROLE 目标也算）。
fn escalation_start_roles(plans: &[SourcePlan]) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = Vec::new();
    for plan in plans {
        if let SourcePlan::Roles(role_ids) = plan {
            for id in role_ids {
                if !out.contains(id) {
                    out.push(*id);
                }
            }
        }
    }
    out
}

// ==================== 节点兜底人（P3-03 · ARCH-8 第 ③ 层） ====================
//
// 场景：角色只有制单人一人（SKIP 剔光）**且**该角色没有上级（`escalation_role_id` 为 NULL）
// ⇒ 升级链走完仍然 0 人。P3-02 之后这里只能 422，单据永远发不出去（ARCH-8 场景 C）。
// 现在由**节点**自己配一个「兜底审批人 / 兜底角色」接管。
//
// 三条纪律（卡面 §2 / 决策 D3）：
//
// 1. **兜底不是免审**：兜底人同样要过停用 / ROBOT / 自审过滤。兜底人就是发起人时
//    **不许放行**，而是 422（报文点名「兜底人即发起人，内控不允许自审」）——
//    否则「配个兜底人」就成了「给自己开一条免审通道」；
// 2. **配错要吵（65042）**：用户已删 / 已停用 / 是机器人（节点没开 `allow_robot`）/
//    角色已删 / 角色 0 可用成员 —— 静默当成「没配兜底」会让运维以为已经兜住了；
// 3. **两列二选一**：DB 有 CHECK，发布期 R22 再挡一次（要多人就配角色）。

/// 派单规则留痕值：按角色展开（`assignee_source='ROLE'` 的普通路径）。
pub const ASSIGNED_BY_ROLE: &str = "ROLE";

/// 派单规则留痕值：沿角色升级链改派（P3-02）。
pub const ASSIGNED_BY_ESCALATION: &str = "ESCALATED";

/// 派单规则留痕值：升级链走完仍无人 → 节点兜底人接管（P3-03）。
pub const ASSIGNED_BY_FALLBACK: &str = "FALLBACK";

/// 派单规则留痕值：委托（P3-04 预留；列已建好，P3-04 不再加列）。
pub const ASSIGNED_BY_DELEGATION: &str = "DELEGATED";

/// 一次兜底尝试的痕迹（报文 / trace / 结构化诊断用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fallback {
    /// 配置的目标名（用户姓名 / 角色名）；空 = 没配。
    pub target: String,
    /// 是否按**角色**兜（`false` = 按用户）。
    pub by_role: bool,
    /// 是否真的派上了。
    pub applied: bool,
    /// 派上 / 没派上的原因（人读）。
    pub reason: String,
}

impl Fallback {
    /// 目标的人读标签：`张三（兜底审批人）` / `销售主管（兜底角色）`。
    pub fn target_label(&self) -> String {
        if self.by_role {
            format!("{}（兜底角色）", self.target)
        } else {
            format!("{}（兜底审批人）", self.target)
        }
    }
}

/// 兜底的四种结局。
#[derive(Debug)]
pub enum FallbackOutcome {
    /// 两列都空 —— 不兜底（行为与 P3-02 之后逐字一致）。
    NotConfigured,
    /// 兜底生效（附「他们是谁」）。
    Applied(Fallback, Vec<Candidate>),
    /// 兜底配置无效（`65042`）：用户不存在 / 停用 / 机器人 / 角色不存在 / 角色无可用成员。
    Invalid(ApiError),
    /// 兜底人就是发起人（内控不允许自审）→ `422 + 65018`；附目标名给报文用。
    SelfConflict(ApiError, Fallback),
}

/// 快照里的兜底列（`String`）→ `Option<Uuid>`；空串 / 非法值都当「没配」。
///
/// 非法值不会静默吞掉：调用方看到两列都解析成 `None` 但原值非空时，报 `65042`
/// （手工改库把 UUID 写坏的情况也要能说清）。
fn parse_fallback_ids(node: &SnapshotNode) -> (Option<Uuid>, Option<Uuid>) {
    let one = |raw: Option<&str>| {
        raw.map(str::trim)
            .filter(|text| !text.is_empty())
            .and_then(|text| Uuid::parse_str(text).ok())
    };
    (
        one(node.fallback_user_id.as_deref()),
        one(node.fallback_role_id.as_deref()),
    )
}

/// 第 ⑦ 步：升级链走完仍无人 → 节点兜底人 / 兜底角色接管（P3-03）。
///
/// # 判定顺序（不可换）
///
/// 1. 两列都空 → [`FallbackOutcome::NotConfigured`]（交 `empty_assignee` 处置）；
/// 2. **用户**兜底：查人（软删不返回）→ 停用 / ROBOT / 自审三道过滤 →
///    发起人本人 → [`FallbackOutcome::SelfConflict`]（内控，**不放行**）；
/// 3. **角色**兜底：展开在位成员 → 同样三道过滤（固定 `SelfPolicy::Skip`）→
///    0 人可用 → `65042`（报文说清「在位几人、被谁过滤掉」）。
///
/// # 为什么用户兜底还要跑 `filter_candidates`
///
/// 不用它就得手写一遍停用 / ROBOT / 自审三判断 —— 那正是「两份实现迟早漂移」的经典形态
/// （升级链已经因为同一个理由每级重跑 `filter_candidates`）。这里复用同一函数，
/// 口径与主路 / 升级链逐字一致。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；配置问题 → [`FallbackOutcome::Invalid`] / `SelfConflict`（不是 500）。
pub async fn apply_fallback(
    ex: &impl SqlExecutor,
    node: &SnapshotNode,
    allow_robot: bool,
    initiator: Uuid,
    trace: &mut Vec<String>,
) -> DbResult<FallbackOutcome> {
    let (user_id, role_id) = parse_fallback_ids(node);

    // 原值非空但解析不出 UUID：手工改库写坏了，必须吵（不能当成「没配兜底」）
    if user_id.is_none() && role_id.is_none() {
        if node.fallback_user_id.is_some() || node.fallback_role_id.is_some() {
            let raw = node
                .fallback_user_id
                .as_deref()
                .or(node.fallback_role_id.as_deref())
                .unwrap_or_default();
            return Ok(FallbackOutcome::Invalid(ApiError::new(
                422,
                error_code::FLOW_FALLBACK_INVALID,
                format!(
                    "节点「{}」的兜底人配置无效：{raw} 不是合法的人员 / 角色 id\
                     （请到「流程定义 → 本流程 → 节点「{}」」重新选择）",
                    node.name, node.name
                ),
            )));
        }
        trace.push("节点未配置兜底人（fallback_user_id / fallback_role_id 均为空）".to_string());
        return Ok(FallbackOutcome::NotConfigured);
    }

    // ---------- 用户兜底 ----------
    if let Some(user_id) = user_id {
        let mut candidates = load_users(ex, &[user_id], AssigneeKind::User).await?;
        let Some(candidate) = candidates.pop() else {
            return Ok(FallbackOutcome::Invalid(ApiError::new(
                422,
                error_code::FLOW_FALLBACK_INVALID,
                format!(
                    "节点「{}」的兜底审批人 {user_id} 不存在（已被删除或 id 非法）。\
                     请到「流程定义 → 本流程 → 节点「{}」」的「兜底审批人」重新选择，\
                     或改配「兜底角色」（要多人兜底就配角色）",
                    node.name, node.name
                ),
            )));
        };
        // ROBOT：兜底是「人手兜底」，节点没开 allow_robot 时机器人不能顶（与隐式来源同一口径）
        if candidate.is_robot && !allow_robot {
            return Ok(FallbackOutcome::Invalid(ApiError::new(
                422,
                error_code::FLOW_FALLBACK_INVALID,
                format!(
                    "节点「{}」的兜底审批人「{}」是机器人，而本节点没有开启「允许机器人」\
                     ——兜底要的是能签字的人。请换成真人，或在节点上打开「允许机器人」",
                    node.name, candidate.name
                ),
            )));
        }
        let mut notes: Vec<String> = Vec::new();
        let filtered = match filter_candidates(
            vec![candidate],
            SelfPolicy::Skip,
            allow_robot,
            initiator,
            &mut notes,
        ) {
            Ok(filtered) => filtered,
            // Skip 策略不会产生 65019；真出现说明策略被改坏了，按「不可用」处理并留痕
            Err(error) => return Ok(FallbackOutcome::Invalid(error)),
        };

        // 自审：兜底人 == 发起人 → 内控不允许，直接 422（不是「静默换人」也不是「放行」）
        if filtered.skipped.iter().any(|c| c.user_id == initiator) {
            let name = filtered
                .skipped
                .first()
                .map(|c| c.name.clone())
                .unwrap_or_default();
            trace.push(format!(
                "兜底人「{name}」就是发起人本人，内控不允许自审 → 422"
            ));
            return Ok(FallbackOutcome::SelfConflict(
                ApiError::new(
                    422,
                    error_code::FLOW_NO_ASSIGNEE,
                    format!(
                        "节点「{}」的兜底审批人「{name}」就是发起人本人 —— \
                         兜底人即发起人，内控不允许自审（兜底不是免审）。\
                         请把兜底人换成非发起人、非制单人的在职人员，\
                         或改配「兜底角色」并确保该角色里有非发起人成员",
                        node.name
                    ),
                ),
                Fallback {
                    target: name,
                    by_role: false,
                    applied: false,
                    reason: "兜底人即发起人，内控不允许自审".to_string(),
                },
            ));
        }

        let Some(candidate) = filtered.approvers.into_iter().next() else {
            // 停用 / 机器人被过滤掉：配置无效
            return Ok(FallbackOutcome::Invalid(ApiError::new(
                422,
                error_code::FLOW_FALLBACK_INVALID,
                format!(
                    "节点「{}」的兜底审批人不可用：{}。\
                     请为他恢复启用状态，或到「流程定义 → 本流程 → 节点「{}」」换一个人 / 改配兜底角色",
                    node.name,
                    if notes.is_empty() {
                        "已停用或不可用".to_string()
                    } else {
                        notes.join("；")
                    },
                    node.name
                ),
            )));
        };
        trace.push(format!(
            "兜底人「{}」接管（第 ⑦ 步：升级链走完仍无人）",
            candidate.name
        ));
        return Ok(FallbackOutcome::Applied(
            Fallback {
                target: candidate.name.clone(),
                by_role: false,
                applied: true,
                reason: "兜底审批人接管".to_string(),
            },
            vec![candidate],
        ));
    }

    // ---------- 角色兜底 ----------
    let role_id = role_id.expect("上面已保证 user/role 至少有一个");
    let Some(role) = load_role_chain_node(ex, role_id).await? else {
        return Ok(FallbackOutcome::Invalid(ApiError::new(
            422,
            error_code::FLOW_FALLBACK_INVALID,
            format!(
                "节点「{}」的兜底角色 {role_id} 不存在（已被删除）。\
                 请到「流程定义 → 本流程 → 节点「{}」」重新选择角色，或改配「兜底审批人」",
                node.name, node.name
            ),
        )));
    };
    let candidates = load_role_members(ex, &[role_id]).await?;
    let member_rows = candidates.len();
    let mut notes: Vec<String> = Vec::new();
    let filtered = match filter_candidates(
        candidates,
        // 兜底同样不许自审：发起人在兜底角色里也只会落 SKIPPED 留痕，不会被塞回待办
        SelfPolicy::Skip,
        allow_robot,
        initiator,
        &mut notes,
    ) {
        Ok(filtered) => filtered,
        Err(error) => return Ok(FallbackOutcome::Invalid(error)),
    };
    if filtered.approvers.is_empty() {
        return Ok(FallbackOutcome::Invalid(ApiError::new(
            422,
            error_code::FLOW_FALLBACK_INVALID,
            format!(
                "节点「{}」的兜底角色「{}」没有可用成员（在位 {member_rows} 人{detail}）。\
                 兜底不是免审：兜底角色里的成员同样不允许是发起人本人 / 已停用 / 机器人。\
                 请给该角色加一个既不是制单人也不是发起人的在职成员",
                node.name,
                role.role_name,
                detail = if notes.is_empty() {
                    String::new()
                } else {
                    format!("：{}", notes.join("；"))
                }
            ),
        )));
    }
    trace.push(format!(
        "兜底角色「{}」接管 {} 人（在位 {member_rows} 人，第 ⑦ 步：升级链走完仍无人）",
        role.role_name,
        filtered.approvers.len()
    ));
    Ok(FallbackOutcome::Applied(
        Fallback {
            target: role.role_name,
            by_role: true,
            applied: true,
            reason: format!(
                "兜底角色展开 {} 人（在位 {member_rows} 人）",
                filtered.approvers.len()
            ),
        },
        filtered.approvers,
    ))
}

/// 「为什么还是没人」的处置链人读提示（P3-03）。
///
/// 422 报文必须能回答「系统还替你想过什么」：升级链走到哪一级、兜底人有没有配。
/// 两句合成一句，避免 resolve / engine 两处各拼一遍（文案漂移是最难查的那类不一致）。
///
/// # 「升级链没配」的判据是**一级都没走**，不是 `escalation.is_none()`
///
/// 角色没有上级（`escalation_role_id IS NULL`）时 `escalate()` 同样会返回
/// `Exhausted`（只是 `levels` 为空）—— 那本质上就是「这条链没配」，报文里必须说
/// 「升级链与兜底人均未配置」，而不是让用户去「升级链已走完」里找一个不存在的链路。
///
/// | 升级链 | 兜底人 | 文案 |
/// |---|---|---|
/// | 没配（或一级都没走） | 没配 | `升级链与兜底人均未配置` |
/// | 走完仍无人 | 没配 | `升级链已走完：<链路>；兜底人未配置` |
/// | 没配 | 配了但不可用 | `兜底人「X」不可用（<原因>）` |
/// | 走完仍无人 | 配了但不可用 | `升级链已走完：<链路>；兜底人「X」不可用（<原因>）` |
pub fn dispatch_hint(escalation: Option<&Escalation>, fallback: Option<&Fallback>) -> String {
    let escalation_text = escalation
        .filter(|esc| !esc.applied && !esc.levels.is_empty())
        .map(|esc| format!("升级链已走完：{}", esc.chain_text()));
    let fallback_text = fallback.map(|fb| {
        if fb.applied {
            format!("兜底人「{}」也已不可用", fb.target)
        } else {
            format!(
                "兜底人「{}」不可用（{}）",
                fb.target,
                if fb.reason.is_empty() {
                    "未生效"
                } else {
                    fb.reason.as_str()
                }
            )
        }
    });
    match (escalation_text, fallback_text) {
        (None, None) => "升级链与兜底人均未配置".to_string(),
        (Some(chain), None) => format!("{chain}；兜底人未配置"),
        (None, Some(fb)) => fb,
        (Some(chain), Some(fb)) => format!("{chain}；{fb}"),
    }
}

/// `DEPT_LEADER`：指定部门的负责人。
async fn load_dept_leaders(ex: &impl SqlExecutor, dept_ids: &[Uuid]) -> DbResult<Vec<Candidate>> {
    if dept_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            "SELECT d.id AS ref_id, d.leader_id AS leader_id \
               FROM sys_dept d \
              WHERE d.id = ANY(?::text::uuid[]) AND d.deleted_at IS NULL AND d.leader_id IS NOT NULL"
                .to_string(),
            vec![array_param(dept_ids)],
        )
        .await?;
    let mut out = Vec::new();
    for row in &rows {
        let leader: Option<Uuid> = row.get_typed("leader_id")?;
        let Some(leader) = leader else { continue };
        let ref_id: Option<Uuid> = row.get_typed("ref_id")?;
        let mut users = load_users(ex, &[leader], AssigneeKind::DeptLeader).await?;
        for user in users.iter_mut() {
            user.source_ref = ref_id;
        }
        out.append(&mut users);
    }
    out.sort_by_key(|candidate| (candidate.user_id, candidate.source_ref));
    Ok(out)
}

/// `INITIATOR_SUPERIOR`：沿 `dept_path` 上溯 `levels` 级取 `leader_id`
/// （**不是** `parent_id` 递归 —— `dept_path` 已是物化路径，一次查完，架构 §4.3.1）。
///
/// `levels = 1` 是「发起人所在部门的负责人」；若发起人自己就是该部门负责人 →
/// 命中自审，走 `self_approval` 策略（坑 3 的第一种形态）。
async fn load_superior(
    ex: &impl SqlExecutor,
    ctx: &InstanceCtx,
    levels: i16,
    trace: &mut Vec<String>,
) -> DbResult<Vec<Candidate>> {
    let rows = ex
        .query_sql(
            "WITH me AS ( \
                 SELECT u.dept_id AS dept_id, d.dept_path AS dept_path \
                   FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id \
                  WHERE u.id = ?::uuid AND u.deleted_at IS NULL \
             ) \
             SELECT d.id AS dept_id, d.leader_id AS leader_id, length(d.dept_path) AS path_len \
               FROM me, sys_dept d \
              WHERE me.dept_path IS NOT NULL \
                AND d.deleted_at IS NULL \
                AND d.dept_path IS NOT NULL \
                AND (d.dept_path = me.dept_path OR me.dept_path LIKE d.dept_path || '%') \
              ORDER BY length(d.dept_path) DESC"
                .to_string(),
            vec![Value::from(&ctx.initiator_id)],
        )
        .await?;
    if rows.is_empty() {
        trace.push("INITIATOR_SUPERIOR：发起人没有部门或部门路径缺失 → 0 行".to_string());
        return Ok(Vec::new());
    }
    let index = (levels.max(1) - 1) as usize;
    let Some(row) = rows.get(index) else {
        trace.push(format!(
            "INITIATOR_SUPERIOR：上溯 {levels} 级超出部门树深度（仅 {} 级）→ 0 行",
            rows.len()
        ));
        return Ok(Vec::new());
    };
    let dept_id: Uuid = row.get_typed("dept_id")?;
    let leader: Option<Uuid> = row.get_typed("leader_id")?;
    match leader {
        None => {
            trace.push(format!(
                "INITIATOR_SUPERIOR：第 {levels} 级部门（{dept_id}）没有负责人 → 0 行"
            ));
            Ok(Vec::new())
        }
        Some(leader) => {
            let mut users = load_users(ex, &[leader], AssigneeKind::InitiatorSuperior).await?;
            for user in users.iter_mut() {
                user.source_ref = Some(dept_id);
            }
            trace.push(format!(
                "INITIATOR_SUPERIOR：第 {levels} 级部门（{dept_id}）负责人命中 {} 行",
                users.len()
            ));
            Ok(users)
        }
    }
}

/// 关联任务的某一方（只有 `biz_type = TASK_ACCEPT` 且 `biz_id` 非空才解析，
/// 否则**返回空** → 走空审批人策略，不静默通过）。
async fn task_party_ids(
    ex: &impl SqlExecutor,
    ctx: &InstanceCtx,
    party: TaskParty,
) -> DbResult<Vec<Uuid>> {
    if !ctx.biz_type.eq_ignore_ascii_case("TASK_ACCEPT") {
        return Ok(Vec::new());
    }
    let Some(biz_id) = ctx.biz_id else {
        return Ok(Vec::new());
    };
    let column = match party {
        TaskParty::Owner => "t.owner_id",
        TaskParty::Creator => "t.creator_id",
        TaskParty::Assigner => "t.assigner_id",
    };
    let sql = format!(
        "SELECT {column} AS party_id FROM biz_task t \
          WHERE t.id = ?::uuid AND t.deleted_at IS NULL"
    );
    let rows = ex.query_sql(sql, vec![Value::from(&biz_id)]).await?;
    match rows.first() {
        None => Ok(Vec::new()),
        Some(row) => {
            let found: Option<Uuid> = row.get_typed("party_id")?;
            Ok(found.into_iter().collect())
        }
    }
}

/// 平台超管（`role_code = SUPER_ADMIN` 且未删未停）。
async fn load_admins(ex: &impl SqlExecutor) -> DbResult<Vec<Candidate>> {
    let rows = ex
        .query_sql(
            "SELECT DISTINCT u.id AS user_id, \
                    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, \
                    u.user_type AS user_type, (u.status = 1) AS active, r.id AS ref_id \
               FROM sys_user u \
               JOIN sys_user_role ur ON ur.user_id = u.id \
               JOIN sys_role r ON r.id = ur.role_id \
              WHERE r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL \
              ORDER BY u.id"
                .to_string(),
            Vec::new(),
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let user_type: String = row.get_typed("user_type")?;
        out.push(Candidate {
            user_id: row.get_typed("user_id")?,
            name: row.get_typed("name")?,
            source: AssigneeKind::Admin,
            source_ref: row.get_typed("ref_id")?,
            is_robot: user_type.eq_ignore_ascii_case("ROBOT"),
            active: row.get_typed("active")?,
        });
    }
    out.sort_by_key(|candidate| candidate.user_id);
    out.dedup_by_key(|candidate| candidate.user_id);
    Ok(out)
}

/// 从 `form_data` 取 `FORM_VAR`（P0 仅 demo：接受单个 uuid 字符串或 uuid 数组）。
pub fn form_var_ids(form_data: &Json, key: &str) -> Vec<Uuid> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Some(value) = form_data.get(trimmed) else {
        return Vec::new();
    };
    let mut out: Vec<Uuid> = Vec::new();
    match value {
        Json::String(text) => {
            if let Ok(id) = Uuid::parse_str(text.trim()) {
                out.push(id);
            }
        }
        Json::Array(items) => {
            for item in items {
                if let Json::String(text) = item
                    && let Ok(id) = Uuid::parse_str(text.trim())
                {
                    out.push(id);
                }
            }
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out
}

/// 名字快照用的「一条待办行的来源展示」（`USER` → `指定用户` 这类）。
pub fn source_label(source: AssigneeKind) -> &'static str {
    match source {
        AssigneeKind::User => "指定用户",
        AssigneeKind::Role => "角色成员",
        AssigneeKind::Initiator => "发起人",
        AssigneeKind::InitiatorSuperior => "发起人主管",
        AssigneeKind::DeptLeader => "部门负责人",
        AssigneeKind::TaskOwner => "任务负责人",
        AssigneeKind::TaskCreator => "任务创建人",
        AssigneeKind::TaskAssigner => "任务分派人",
        AssigneeKind::FormVar => "表单变量",
        AssigneeKind::Admin => "平台管理员",
        // 表达式命中的候选 source 永远是**底层来源**（D-E5），这一支只在防御性展示时用到
        AssigneeKind::Expression => "表达式",
    }
}

/// 便于日志拼接的 `Option<Uuid>` 文本。
pub fn opt_uuid_text(id: Option<Uuid>) -> Value {
    opt_text(id.map(|value| value.to_string()).as_deref())
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::graph::SnapshotAssignee;

    fn node(assignee_type: &str) -> SnapshotNode {
        SnapshotNode {
            key: "n1".to_string(),
            name: "审批".to_string(),
            node_type: "APPROVAL".to_string(),
            assignee_type: Some(assignee_type.to_string()),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: Some("approver".to_string()),
            superior_levels: 2,
            empty_assignee: "SKIP".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            config: Json::Object(Default::default()),
            assignees: Vec::new(),
        }
    }

    fn with_assignee(mut node: SnapshotNode, kind: &str, id: Uuid) -> SnapshotNode {
        node.assignees.push(SnapshotAssignee {
            assignee_type: kind.to_string(),
            ref_id: id.to_string(),
            sort_order: 0,
        });
        node
    }

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn eight_sources_map_to_query_plans() {
        // 验收 13「8 种来源各一组」的纯逻辑侧：每种来源都必须翻译成**正确的查询计划**。
        let role = uid(7);
        let dept = uid(8);
        let user = uid(9);

        assert_eq!(
            plan_of(&with_assignee(node("USER"), "USER", user)),
            SourcePlan::Users(vec![user])
        );
        assert_eq!(
            plan_of(&with_assignee(node("ROLE"), "ROLE", role)),
            SourcePlan::Roles(vec![role])
        );
        assert_eq!(plan_of(&node("INITIATOR")), SourcePlan::Initiator);
        assert_eq!(
            plan_of(&node("INITIATOR_SUPERIOR")),
            SourcePlan::InitiatorSuperior(2),
            "superior_levels 必须带过去（1 = 本部门负责人）"
        );
        assert_eq!(
            plan_of(&with_assignee(node("DEPT_LEADER"), "DEPT_LEADER", dept)),
            SourcePlan::DeptLeaders(vec![dept])
        );
        assert_eq!(
            plan_of(&node("TASK_OWNER")),
            SourcePlan::TaskParty(TaskParty::Owner)
        );
        assert_eq!(
            plan_of(&node("TASK_CREATOR")),
            SourcePlan::TaskParty(TaskParty::Creator)
        );
        assert_eq!(
            plan_of(&node("TASK_ASSIGNER")),
            SourcePlan::TaskParty(TaskParty::Assigner)
        );
        assert_eq!(
            plan_of(&node("FORM_VAR")),
            SourcePlan::FormVar("approver".to_string())
        );
        assert_eq!(plan_of(&node("ADMIN")), SourcePlan::Admins);

        // EXPRESSION 是 P1：必须落到 Unsupported（调用方返回 501），绝不能当空审批人
        assert_eq!(
            plan_of(&node("EXPRESSION")),
            SourcePlan::Unsupported("EXPRESSION".to_string())
        );
    }

    #[test]
    fn initiator_superior_levels_are_clamped() {
        let mut spec = node("INITIATOR_SUPERIOR");
        spec.superior_levels = 0;
        assert_eq!(plan_of(&spec), SourcePlan::InitiatorSuperior(1));
        spec.superior_levels = 99;
        assert_eq!(plan_of(&spec), SourcePlan::InitiatorSuperior(10));
    }

    #[test]
    fn three_layer_dedupe_collapses_same_user() {
        // 验收 14：一个人同时是「角色成员 + 发起人主管」→ 只生成 1 行待办。
        let initiator = uid(1);
        let same = uid(2);
        let candidates = vec![
            Candidate::new(same, "张三", AssigneeKind::Role).with_ref(Some(uid(30))),
            Candidate::new(same, "张三", AssigneeKind::InitiatorSuperior).with_ref(Some(uid(31))),
            Candidate::new(uid(3), "李四", AssigneeKind::Role),
        ];
        let out = finalize(
            candidates,
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(out.error.is_none());
        assert_eq!(out.approvers.len(), 2, "3 条候选去重成 2 个人");
        let ids: Vec<Uuid> = out.approvers.iter().map(|c| c.user_id).collect();
        assert_eq!(ids, vec![uid(2), uid(3)]);
        assert!(
            out.notes.iter().any(|note| note.contains("已去重")),
            "去重必须留痕：{:?}",
            out.notes
        );
    }

    #[test]
    fn explicit_user_wins_over_implicit_source_when_deduping() {
        let initiator = uid(1);
        let same = uid(2);
        let candidates = vec![
            Candidate::new(same, "张三", AssigneeKind::Role),
            Candidate::new(same, "张三", AssigneeKind::User),
        ];
        let out = finalize(
            candidates,
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(out.approvers.len(), 1);
        assert_eq!(
            out.approvers[0].source,
            AssigneeKind::User,
            "去重时显式 USER 优先（D18 的机器人放行也靠它）"
        );
    }

    #[test]
    fn self_approval_skip_keeps_a_skipped_row_and_counts() {
        // 验收 15：发起人 = 部门主管 → 落一条 SKIPPED 待办（不是静默），且节点自动通过。
        let initiator = uid(1);
        let candidates = vec![Candidate::new(
            initiator,
            "我自己",
            AssigneeKind::DeptLeader,
        )];
        let out = finalize(
            candidates,
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(out.error.is_none());
        assert!(out.approvers.is_empty());
        assert_eq!(out.skipped.len(), 1, "SKIPPED 必须留痕（坑 3）");
        assert_eq!(out.skipped[0].user_id, initiator);
        assert!(out.auto_pass, "没有别的审批人 → 节点自动跳过并计数");
        assert!(out.node_is_skipped());
    }

    #[test]
    fn self_approval_skip_with_other_approvers_keeps_node_active() {
        let initiator = uid(1);
        let candidates = vec![
            Candidate::new(initiator, "我自己", AssigneeKind::Role),
            Candidate::new(uid(2), "别的人", AssigneeKind::Role),
        ];
        let out = finalize(
            candidates,
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(out.approvers.len(), 1, "发起人被剔除后仍有 1 个审批人");
        assert_eq!(out.skipped.len(), 1);
        assert!(!out.auto_pass, "还有人审 → 节点不能自动跳过");
    }

    #[test]
    fn self_approval_allow_and_forbid() {
        let initiator = uid(1);
        let candidates = vec![Candidate::new(initiator, "我自己", AssigneeKind::User)];
        let allow = finalize(
            candidates.clone(),
            SelfPolicy::Allow,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(allow.approvers.len(), 1, "ALLOW：保留（UI 打自审标）");
        assert!(allow.skipped.is_empty());

        let forbid = finalize(
            candidates,
            SelfPolicy::Forbid,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        let error = forbid.error.expect("FORBID 必须报错");
        assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert_eq!(error.status, 422);
    }

    #[test]
    fn empty_assignee_policies() {
        // 验收 16：角色里 0 人 → 节点跳过 + auto_passed_nodes + 1（调用方计数）
        let initiator = uid(1);
        let skip = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(skip.auto_pass);
        assert!(skip.error.is_none());

        let error = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::Error,
            false,
            initiator,
        );
        assert_eq!(
            error.error.expect("ERROR 必须报错").code,
            error_code::FLOW_NO_ASSIGNEE
        );

        let fallback = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::FallbackAdmin,
            false,
            initiator,
        );
        assert!(fallback.fallback_admin);
        assert!(!fallback.auto_pass, "FALLBACK_ADMIN 不是自动通过");

        // 卡面写作 FORBID 的口径必须等价于 ERROR（库里只有 ERROR）
        assert_eq!(EmptyPolicy::parse("FORBID"), EmptyPolicy::Error);
        assert_eq!(EmptyPolicy::parse("error"), EmptyPolicy::Error);
        assert_eq!(EmptyPolicy::parse("随便写"), EmptyPolicy::Skip);
    }

    #[test]
    fn robot_is_filtered_for_implicit_sources_but_allowed_for_explicit_user() {
        // 验收 18 / 19（D18）
        let initiator = uid(1);
        let robot = Candidate {
            user_id: uid(9),
            name: "报表机器人".to_string(),
            source: AssigneeKind::Role,
            source_ref: Some(uid(30)),
            is_robot: true,
            active: true,
        };
        let filtered = finalize(
            vec![robot.clone()],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(
            filtered.approvers.is_empty(),
            "隐式来源里的 ROBOT 必须被过滤"
        );
        assert!(filtered.auto_pass);
        assert!(filtered.notes.iter().any(|note| note.contains("机器人")));

        let explicit = Candidate {
            source: AssigneeKind::User,
            source_ref: None,
            ..robot.clone()
        };
        let allowed = finalize(
            vec![explicit],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(allowed.approvers.len(), 1, "显式 USER 指定 ROBOT 必须放行");

        let node_allows = finalize(
            vec![robot],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            true,
            initiator,
        );
        assert_eq!(
            node_allows.approvers.len(),
            1,
            "allow_robot=true 时隐式来源也放行"
        );
    }

    #[test]
    fn disabled_user_is_dropped_with_note() {
        let initiator = uid(1);
        let candidate = Candidate {
            user_id: uid(5),
            name: "已停用".to_string(),
            source: AssigneeKind::Role,
            source_ref: None,
            is_robot: false,
            active: false,
        };
        let out = finalize(
            vec![candidate],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(out.approvers.is_empty());
        assert!(out.notes.iter().any(|note| note.contains("已停用")));
    }

    #[test]
    fn form_var_accepts_scalar_and_array() {
        let one = uid(11);
        let two = uid(12);
        let scalar = serde_json::json!({ "approver": one.to_string() });
        assert_eq!(form_var_ids(&scalar, "approver"), vec![one]);

        let array = serde_json::json!({ "approver": [one.to_string(), two.to_string(), "乱写"] });
        assert_eq!(form_var_ids(&array, "approver"), vec![one, two]);

        assert!(form_var_ids(&scalar, "missing").is_empty());
        assert!(form_var_ids(&scalar, "  ").is_empty());
    }

    #[test]
    fn array_param_uses_text_literal() {
        // 本栈驱动不支持 Value::Array，必须转 `{uuid,uuid}` 文本（scope.rs 同款）
        let value = array_param(&[uid(1), uid(2)]);
        match value {
            Value::Text(text) => assert!(text.starts_with('{') && text.contains(',')),
            other => panic!("必须是文本字面量，实际 {other:?}"),
        }
    }
}

// ==================== EXPRESSION 单测（批次 D · P1-A2） ====================

/// EXPRESSION 分支的单测（独立模块：不动既有的 8 来源 / 三层去重用例）。
///
/// 覆盖卡面「二、单测要求 ①」的 6 条 + 政策矩阵与 trace 断言：
/// 命中第一条 / 首条即定 / 无命中三策略 / 求值出错 / 命中但 0 人 / 底层来源 / 规则 id。
#[cfg(test)]
mod expression_tests {
    use super::*;
    use serde_json::json;

    const FINANCE: &str = "FINANCE_DIRECTOR";

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// `form.amount > 10000` → 财务总监；`form.amount >= 0` → 指定用户；`otherwise` → 发起人。
    fn expression_json() -> Json {
        json!({
            "version": 1,
            "rules": [
                {
                    "id": "r_big",
                    "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10000},
                    "then": [{"type": "ROLE", "role_codes": [FINANCE]}]
                },
                {
                    "id": "r_small",
                    "when": {"op": "cmp", "field": "form.amount", "cmp": "gte", "value": 0},
                    "then": [{"type": "USER", "user_ids": [uid(9).to_string()]}]
                }
            ],
            "otherwise": [{"type": "INITIATOR"}]
        })
    }

    fn config(expression: Json) -> Json {
        json!({ "expression": expression })
    }

    fn eval(amount: Option<i64>) -> EvalContext {
        let mut ctx = EvalContext::default();
        if let Some(amount) = amount {
            ctx.form.insert("amount".to_string(), json!(amount));
        }
        ctx
    }

    fn resolve_ctx() -> ResolveContext {
        ResolveContext {
            initiator_dept_id: Some(uid(8)),
        }
    }

    // ---------- ① 命中第一条规则 → 用该规则的 then 目标 ----------

    #[test]
    fn expression_hits_first_rule_and_uses_its_then() {
        let plans = expression_plans(
            &config(expression_json()),
            &eval(Some(12000)),
            &resolve_ctx(),
        )
        .expect("合法表达式必须解析成功");

        assert_eq!(
            plans.rule_id.as_deref(),
            Some("r_big"),
            "金额 1.2 万必须命中第一条规则 r_big"
        );
        assert_eq!(plans.targets.len(), 1, "r_big 只有一个 target");
        assert_eq!(
            plans.targets[0],
            ResolvedTarget::RoleCodes(vec![FINANCE.to_string()]),
            "ROLE 必须翻译成 RoleCodes（code→id 的查询在 assignee.rs 里做，§8.3 方案 B）"
        );
        assert!(
            plans
                .trace
                .iter()
                .any(|line| line.contains("命中规则 r_big")),
            "trace 必须记录命中了哪条规则（排障「为什么是他」），实际 {:#?}",
            plans.trace
        );
    }

    #[test]
    fn expression_first_hit_wins_and_later_rules_are_not_consulted() {
        // amount=12000 同时满足 r_big 与 r_small；**首条即定**，不许被后者覆盖。
        let plans = expression_plans(
            &config(expression_json()),
            &eval(Some(12000)),
            &resolve_ctx(),
        )
        .expect("合法表达式必须解析成功");
        assert_eq!(plans.rule_id.as_deref(), Some("r_big"));
        assert!(
            !plans.trace.iter().any(|line| line.contains("r_small")),
            "r_small 根本不该被求值（短路 + 首条即定），实际 {:#?}",
            plans.trace
        );
    }

    #[test]
    fn expression_falls_through_to_second_rule_when_first_fails() {
        let plans = expression_plans(&config(expression_json()), &eval(Some(10)), &resolve_ctx())
            .expect("合法表达式必须解析成功");
        assert_eq!(
            plans.rule_id.as_deref(),
            Some("r_small"),
            "金额 10 不满足 r_big，必须落到第二条规则"
        );
        assert_eq!(
            plans.targets,
            vec![ResolvedTarget::Plan(SourcePlan::Users(vec![uid(9)]))]
        );
    }

    // ---------- ② 无命中 → 走 empty_assignee（SKIP / ERROR / FALLBACK_ADMIN） ----------

    fn no_match_expression() -> Json {
        json!({
            "version": 1,
            "rules": [{
                "id": "r_never",
                "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 999999},
                "then": [{"type": "ROLE", "role_codes": [FINANCE]}]
            }]
        })
    }

    #[test]
    fn expression_no_match_without_otherwise_yields_no_target() {
        let plans = expression_plans(
            &config(no_match_expression()),
            &eval(Some(1)),
            &resolve_ctx(),
        )
        .expect("合法表达式必须解析成功");
        assert_eq!(plans.rule_id, None, "没有命中任何规则 → 没有 rule_id");
        assert!(plans.targets.is_empty(), "没有 otherwise → 0 个目标");
        assert!(
            plans
                .trace
                .iter()
                .any(|line| line.contains("无规则命中且无兜底")),
            "trace 必须写明「无规则命中且无兜底 → 空审批人」，实际 {:#?}",
            plans.trace
        );
    }

    #[test]
    fn expression_no_match_follows_empty_policy_skip() {
        // 目标为空 → 候选为空 → finalize 走 empty_assignee=SKIP（自动通过并计数）
        let finalized = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            uid(1),
        );
        assert!(finalized.auto_pass, "SKIP 必须自动通过");
        assert!(finalized.node_is_skipped(), "没有 error ⇒ 节点跳过");
        assert_eq!(finalized.rule_id, None);
    }

    #[test]
    fn expression_no_match_follows_empty_policy_error() {
        let finalized = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::Error,
            false,
            uid(1),
        );
        let error = finalized
            .error
            .as_ref()
            .expect("ERROR 策略必须给出业务拒绝");
        assert_eq!(error.code, error_code::FLOW_NO_ASSIGNEE);
        assert_eq!(error.status, 422);
        assert!(!finalized.node_is_skipped(), "ERROR 不许静默跳过");
    }

    #[test]
    fn expression_no_match_follows_empty_policy_fallback_admin() {
        let finalized = finalize(
            Vec::new(),
            SelfPolicy::Skip,
            EmptyPolicy::FallbackAdmin,
            false,
            uid(1),
        );
        assert!(finalized.fallback_admin, "FALLBACK_ADMIN 必须标记回退超管");
        assert!(finalized.error.is_none());
    }

    // ---------- ③ 求值出错（字段缺失）→ 不算命中 ----------

    #[test]
    fn expression_eval_error_is_treated_as_not_matched() {
        let broken = json!({
            "version": 1,
            "rules": [{
                "id": "r_missing",
                "when": {"op": "cmp", "field": "form.not_filled", "cmp": "gt", "value": 1},
                "then": [{"type": "ROLE", "role_codes": [FINANCE]}]
            }],
            "otherwise": [{"type": "INITIATOR"}]
        });
        let plans = expression_plans(&config(broken), &eval(Some(1)), &resolve_ctx())
            .expect("字段缺失不是解析错，必须照常返回");

        assert_eq!(
            plans.rule_id.as_deref(),
            Some(expression::OTHERWISE_RULE_ID),
            "求值出错 = 该规则不成立，必须继续走下去（落到 otherwise）"
        );
        assert_eq!(
            plans.targets,
            vec![ResolvedTarget::Plan(SourcePlan::Initiator)]
        );
        assert!(
            plans
                .trace
                .iter()
                .any(|line| line.contains("求值失败") && line.contains("r_missing")),
            "失败必须留痕（S4 失败不静默），实际 {:#?}",
            plans.trace
        );
    }

    // ---------- ④ 命中但查出 0 人 → 走 empty_assignee，**不顺延**（D-E6） ----------

    #[test]
    fn expression_hit_but_zero_people_does_not_fall_through() {
        // amount=12000 命中 r_big（财务总监），但假设该角色 0 成员 → 候选为空。
        // D-E6：**不顺延到 r_small**，直接走 empty_assignee，并保留 r_big 作为 rule_id。
        let plans = expression_plans(
            &config(expression_json()),
            &eval(Some(12000)),
            &resolve_ctx(),
        )
        .expect("合法表达式必须解析成功");
        assert_eq!(plans.rule_id.as_deref(), Some("r_big"));
        assert_eq!(plans.targets.len(), 1, "目标存在（只是查不出人）");

        let finalized = finalize(
            Vec::new(), // ← 模拟「ROLE 展开 0 行」
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            uid(1),
        );
        assert!(finalized.auto_pass, "0 人 → 走 empty_assignee=SKIP");
        let mut finalized = finalized;
        finalized.rule_id = plans.rule_id.clone();
        assert_eq!(
            finalized.rule_id.as_deref(),
            Some("r_big"),
            "命中但 0 人时，rule_id 必须留着（审计要能看出走的是哪条规则）"
        );
    }

    // ---------- ⑤ assignee_source 写底层来源，不写 EXPRESSION ----------

    #[test]
    fn expression_candidates_keep_underlying_source() {
        // 表达式命中 ROLE → 待办行的 assignee_source 必须是 ROLE（不是 EXPRESSION）
        let candidates =
            vec![Candidate::new(uid(2), "财务总监", AssigneeKind::Role).with_ref(Some(uid(7)))];
        let finalized = finalize(
            candidates,
            SelfPolicy::Allow,
            EmptyPolicy::Skip,
            false,
            uid(1),
        );
        assert_eq!(finalized.approvers.len(), 1);
        assert_eq!(
            finalized.approvers[0].source,
            AssigneeKind::Role,
            "assignee_source 必须是底层来源（D-E5 注：source_label / 详情页 / 抄送零改动）"
        );
        assert_eq!(finalized.approvers[0].source.as_str(), "ROLE");
        assert_ne!(
            finalized.approvers[0].source,
            AssigneeKind::Expression,
            "绝不能把 EXPRESSION 写进 assignee_source（库 CHECK 也不认）"
        );
    }

    // ---------- ⑥ 表达式缺失 / 保留词 ----------

    #[test]
    fn expression_missing_config_is_unsupported_501() {
        let error = expression_plans(
            &Json::Object(Default::default()),
            &eval(Some(1)),
            &resolve_ctx(),
        )
        .expect_err("EXPRESSION 节点没有 config.expression 必须报错，不能当空审批人");
        assert_eq!(error.status, 501);
        assert_eq!(error.code, error_code::FLOW_EXPRESSION_UNSUPPORTED);
        assert!(
            error.msg.contains("expression"),
            "文案要点名缺什么：{}",
            error.msg
        );
    }

    #[test]
    fn expression_prev_approver_is_rejected_with_65033() {
        let reserved = json!({
            "version": 1,
            "rules": [{
                "id": "r_prev",
                "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 1},
                "then": [{"type": "PREV_APPROVER"}]
            }]
        });
        let error = expression_plans(&config(reserved), &eval(Some(5)), &resolve_ctx())
            .expect_err("保留词 PREV_APPROVER 必须被拒");
        assert_eq!(
            error.status, 400,
            "运行期透传 400（发布期就该拦，这里只兜底）"
        );
        assert_eq!(error.code, error_code::FLOW_EXPRESSION_INVALID);
        assert!(error.msg.contains("PREV_APPROVER"), "{}", error.msg);
    }

    #[test]
    fn expression_invalid_structure_is_501_with_reason() {
        let bad = json!({"version": 99, "rules": []});
        let error = expression_plans(&config(bad), &eval(Some(1)), &resolve_ctx())
            .expect_err("版本不认识必须报错");
        assert_eq!(error.status, 501);
        assert_eq!(error.code, error_code::FLOW_EXPRESSION_UNSUPPORTED);
    }

    // ---------- 目标翻译 / 来源识别 ----------

    #[test]
    fn expression_targets_translate_to_existing_query_plans() {
        let expr = json!({
            "version": 1,
            "rules": [{
                "id": "r_mix",
                "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 1},
                "then": [
                    {"type": "USER", "user_ids": [uid(9).to_string()]},
                    {"type": "DEPT_LEADER"},
                    {"type": "INITIATOR_SUPERIOR", "levels": 2},
                    {"type": "ADMIN"}
                ]
            }]
        });
        let plans = expression_plans(&config(expr), &eval(Some(5)), &resolve_ctx())
            .expect("合法表达式必须解析成功");
        assert_eq!(
            plans.targets,
            vec![
                ResolvedTarget::Plan(SourcePlan::Users(vec![uid(9)])),
                // DEPT_LEADER 未指定部门 → 回落发起人部门（§3.5）
                ResolvedTarget::Plan(SourcePlan::DeptLeaders(vec![uid(8)])),
                ResolvedTarget::Plan(SourcePlan::InitiatorSuperior(2)),
                ResolvedTarget::Plan(SourcePlan::Admins),
            ],
            "多个 target = 并集，顺序必须保持 JSON 数组顺序（影响 finalize 的去重优先级）"
        );
    }

    #[test]
    fn is_expression_recognizes_case_and_rejects_others() {
        let mut spec = SnapshotNode {
            key: "n1".to_string(),
            name: "审批".to_string(),
            node_type: "APPROVAL".to_string(),
            assignee_type: Some(" expression ".to_string()),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: "SKIP".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            config: Json::Object(Default::default()),
            assignees: Vec::new(),
        };
        assert!(is_expression(&spec), "大小写 + 空格必须识别为 EXPRESSION");
        spec.assignee_type = Some("ROLE".to_string());
        assert!(!is_expression(&spec));
        spec.assignee_type = None;
        assert!(!is_expression(&spec), "没配来源不是表达式");
    }
}

// ==================== 升级链单测（P3-02 · ARCH-8 场景 B） ====================

/// 升级链的纯逻辑用例（`decide_level` / `next_frontier` / `register_frontier`）。
///
/// 为什么这些不写成「跑一遍引擎」的集成用例：升级链的判定完全是**内存里的三段纯函数**
/// （每级过滤 / 下一级指针 / 成环检测），把它们单独钉住后，引擎侧只需要验证「接线对不对」，
/// 而接线由真 HTTP 证据（`docs/evidence/*-p302-escalation`）覆盖。
#[cfg(test)]
mod escalation_tests {
    use super::*;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn role(id: Uuid, code: &str, name: &str, parent: Option<Uuid>) -> RoleChainNode {
        RoleChainNode {
            id,
            role_code: code.to_string(),
            role_name: name.to_string(),
            parent,
        }
    }

    fn member(id: Uuid, name: &str, initiator: Uuid) -> Candidate {
        let mut candidate = Candidate::new(id, name, AssigneeKind::Role);
        candidate.source_ref = Some(uid(900));
        // 默认都是「不是发起人」的可用成员；自审场景由调用方把 id 设成 initiator
        let _ = initiator;
        candidate
    }

    /// ① 角色只有发起人 → SKIP → 升级到上级角色（2 人）：本级取到 2 个可用审批人，
    /// 且发起人即使也在上级角色里也不会被塞回待办。
    #[test]
    fn escalation_hits_upper_role_with_two_members() {
        let initiator = uid(1);
        let boss = role(uid(20), "SALES_DIRECTOR", "销售总监", None);
        let members = vec![(
            boss.id,
            vec![
                member(uid(2), "张三", initiator),
                member(uid(3), "李四", initiator),
                // 上级角色里也有发起人本人 → 必须被自审过滤掉，不能回到待办
                member(initiator, "发起人", initiator),
            ],
        )];
        let mut trace = Vec::new();
        let (rows, hit) = decide_level(
            std::slice::from_ref(&boss),
            &members,
            false,
            initiator,
            0,
            &mut trace,
        );
        let approvers = hit.expect("上级角色有 2 个可用成员，必须命中");
        assert_eq!(approvers.len(), 2, "待办 2 行（发起人被剔掉）");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].member_rows, 3, "成员行数（过滤前）");
        assert_eq!(rows[0].usable, 2, "过滤后可用 2 人");
        assert!(trace.iter().any(|line| line.contains("销售总监")));
    }

    /// ② 上级角色也是空 → 本级不命中（继续再上一级）。
    #[test]
    fn escalation_level_without_member_keeps_walking() {
        let initiator = uid(1);
        let empty = role(uid(20), "SALES_SUPERVISOR", "销售主管", Some(uid(30)));
        let mut trace = Vec::new();
        let (rows, hit) = decide_level(
            std::slice::from_ref(&empty),
            &[(empty.id, Vec::new())],
            false,
            initiator,
            0,
            &mut trace,
        );
        assert!(hit.is_none(), "本级 0 人 → 必须继续上溯");
        assert_eq!(rows[0].member_rows, 0);
        assert_eq!(rows[0].usable, 0);
        // 下一级指针来自 `escalation_role_id`
        assert_eq!(next_frontier(std::slice::from_ref(&empty)), vec![uid(30)]);
    }

    /// ③ 升级链成环 → `register_frontier` 认出重复节点（运行时据此报 65040）。
    #[test]
    fn escalation_cycle_is_detected() {
        let mut visited = BTreeSet::new();
        // 起点角色先进 visited
        assert_eq!(register_frontier(&[uid(10)], &mut visited), None);
        // 一级：销售主管
        assert_eq!(register_frontier(&[uid(20)], &mut visited), None);
        // 二级：又回到起点 → 环
        assert_eq!(
            register_frontier(&[uid(10)], &mut visited),
            Some(uid(10)),
            "回到走过的角色必须被判成环（65040）"
        );
    }

    /// ③b 自引用（A→A）同样被判成环。
    #[test]
    fn escalation_self_reference_is_a_cycle() {
        let mut visited = BTreeSet::new();
        assert_eq!(register_frontier(&[uid(10)], &mut visited), None);
        assert_eq!(register_frontier(&[uid(10)], &mut visited), Some(uid(10)));
    }

    /// ④ 走到顶仍 0 人：链路文案里必须能看到每一级的角色名与人数（报文契约）。
    #[test]
    fn escalation_exhausted_chain_text_lists_every_level() {
        let escalation = Escalation {
            reason: ESCALATE_REASON_SKIP.to_string(),
            from_role_code: "SALES_CLERK".to_string(),
            from_role_name: "销售员".to_string(),
            levels: vec![
                EscalationLevel {
                    role_id: uid(20),
                    role_code: "SALES_SUPERVISOR".to_string(),
                    role_name: "销售主管".to_string(),
                    member_rows: 0,
                    usable: 0,
                },
                EscalationLevel {
                    role_id: uid(30),
                    role_code: "GENERAL_MANAGER".to_string(),
                    role_name: "总经理".to_string(),
                    member_rows: 1,
                    usable: 0,
                },
            ],
            applied: false,
        };
        let text = escalation.chain_text();
        assert!(text.contains("销售员"), "{text}");
        assert!(text.contains("销售主管"), "{text}");
        assert!(text.contains("0/0 人"), "成员 0 行要能看出来：{text}");
        assert!(
            text.contains("0/1 人"),
            "有成员但全被过滤也要能看出来：{text}"
        );
        assert_eq!(escalation.reason_label(), "自审跳过");
        assert!(!escalation.applied);
    }

    /// `escalated_from_role` 是 `varchar(40)`：多个起始角色拼接必须按字符截断（不能按字节）。
    #[test]
    fn escalated_from_role_is_truncated_to_column_width() {
        let codes = [
            "SALES_CLERK",
            "PURCHASE_SUPERVISOR",
            "WAREHOUSE_MANAGER",
            "FINANCE_DIRECTOR",
        ];
        let nodes: Vec<RoleChainNode> = codes
            .iter()
            .enumerate()
            .map(|(index, code)| role(uid(index as u128 + 1), code, code, None))
            .collect();
        let joined = join_role_codes(&nodes);
        assert!(
            joined.chars().count() <= ESCALATED_FROM_ROLE_LIMIT,
            "必须 ≤ {ESCALATED_FROM_ROLE_LIMIT} 个字符：{joined}"
        );
        assert!(joined.starts_with("SALES_CLERK"), "截断只发生在尾部");
        assert_eq!(ESCALATED_FROM_ROLE_LIMIT, 40, "与迁移里的列宽一致");
    }

    /// 下一级指针去重 + 保持顺序（一个角色可以被两个下级共同指向上级）。
    #[test]
    fn next_frontier_dedups_and_keeps_order() {
        let parent = uid(99);
        let nodes = vec![
            role(uid(10), "A", "A", Some(parent)),
            role(uid(11), "B", "B", Some(parent)),
            role(uid(12), "C", "C", Some(uid(50))),
            role(uid(13), "D", "D", None),
        ];
        assert_eq!(next_frontier(&nodes), vec![parent, uid(50)]);
    }
}

/// 升级链的**真库**用例（`ERP_DB_URL` 开关，与 `flow::edge_admin_tests` 同口径）。
///
/// 卡面单测 ①–④ 里，只有「沿 `sys_role.escalation_role_id` 真的查到人」这一步需要数据库；
/// 纯判定部分在 [`escalation_tests`] 里已经钉住。这里补的是「SQL 口径与判定接得上」：
/// 角色成员查询（未过期 / 未软删）、`escalation_role_id` 指针、成环与到顶的结局。
///
/// ```bash
/// cd backend && ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_p302_e2e' \
///   cargo test --lib business::flow::assignee::escalation_db_tests -- --nocapture
/// ```
#[cfg(test)]
mod escalation_db_tests {
    use super::*;

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
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 一轮夹具：4 个角色 + 4 个用户（P302UT_* 前缀，结束即清理）。
    struct Fixture {
        maker_role: Uuid,
        mid_role: Uuid,
        boss_role: Uuid,
        dead_role: Uuid,
        initiator: Uuid,
        boss1: Uuid,
        boss2: Uuid,
    }

    async fn setup(tag: &str) -> Fixture {
        // 上一轮跑挂在中间时留下的同名行先清掉（用例必须可重复执行）
        for sql in [
            format!(
                "DELETE FROM sys_user_role WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'P302UT\\_%\\_{tag}')"
            ),
            format!("DELETE FROM sys_user WHERE username LIKE 'p302ut\\_%\\_{tag}'"),
            format!("DELETE FROM sys_role WHERE role_code LIKE 'P302UT\\_%\\_{tag}'"),
        ] {
            exec(&sql).await;
        }
        let maker_role = Uuid::now_v7();
        let mid_role = Uuid::now_v7();
        let boss_role = Uuid::now_v7();
        let dead_role = Uuid::now_v7();
        let initiator = Uuid::now_v7();
        let boss1 = Uuid::now_v7();
        let boss2 = Uuid::now_v7();
        exec(&format!(
            "INSERT INTO sys_role (id, role_code, role_name, status, sort_order) VALUES \
             ('{maker_role}'::uuid, 'P302UT_MAKER_{tag}', 'P3-02 夹具-发起人角色', 1, 900), \
             ('{mid_role}'::uuid, 'P302UT_MID_{tag}', 'P3-02 夹具-中间空角色', 1, 901), \
             ('{boss_role}'::uuid, 'P302UT_BOSS_{tag}', 'P3-02 夹具-上级角色', 1, 902), \
             ('{dead_role}'::uuid, 'P302UT_DEAD_{tag}', 'P3-02 夹具-链尾', 1, 903)"
        ))
        .await;
        // 链条：发起人角色 →（空）→ 上级角色（两条 UPDATE 分开执行：驱动不接受多语句）
        exec(&format!(
            "UPDATE sys_role SET escalation_role_id = '{mid_role}'::uuid WHERE id = '{maker_role}'::uuid"
        ))
        .await;
        exec(&format!(
            "UPDATE sys_role SET escalation_role_id = '{boss_role}'::uuid WHERE id = '{mid_role}'::uuid"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
             ('{initiator}'::uuid, 'p302ut_init_{tag}', 'x', 'P3-02 发起人', 1), \
             ('{boss1}'::uuid, 'p302ut_boss1_{tag}', 'x', 'P3-02 上级甲', 1), \
             ('{boss2}'::uuid, 'p302ut_boss2_{tag}', 'x', 'P3-02 上级乙', 1)"
        ))
        .await;
        // 发起人是「发起人角色」的唯一成员；上级角色两人（其中一个**也是发起人**，必须被过滤掉）
        exec(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES \
             ('{initiator}'::uuid, '{maker_role}'::uuid), \
             ('{boss1}'::uuid, '{boss_role}'::uuid), \
             ('{boss2}'::uuid, '{boss_role}'::uuid), \
             ('{initiator}'::uuid, '{boss_role}'::uuid)"
        ))
        .await;
        Fixture {
            maker_role,
            mid_role,
            boss_role,
            dead_role,
            initiator,
            boss1,
            boss2,
        }
    }

    async fn teardown(fx: &Fixture) {
        // 清理（每条语句单独执行：驱动不接受多语句 prepared）
        for sql in [
            format!(
                "DELETE FROM sys_user_role WHERE user_id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid)",
                fx.initiator, fx.boss1, fx.boss2
            ),
            format!(
                "DELETE FROM sys_user WHERE id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid)",
                fx.initiator, fx.boss1, fx.boss2
            ),
            format!(
                "UPDATE sys_role SET escalation_role_id = NULL WHERE id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid,'{3}'::uuid)",
                fx.maker_role, fx.mid_role, fx.boss_role, fx.dead_role
            ),
            format!(
                "DELETE FROM sys_role WHERE id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid,'{3}'::uuid)",
                fx.maker_role, fx.mid_role, fx.boss_role, fx.dead_role
            ),
        ] {
            exec(&sql).await;
        }
    }

    /// ① 角色只有发起人 → 升级到上一级（跳过 0 人的中间级）→ 2 个可用审批人；
    ///    上级角色里的发起人本人被剔掉，`escalated_from_role` 是**起始**角色的编码。
    #[tokio::test]
    async fn escalate_skips_empty_level_and_filters_initiator() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("a").await;
        let mut trace = Vec::new();
        let outcome = escalate(
            db(),
            &[fx.maker_role],
            ESCALATE_REASON_SKIP,
            false,
            fx.initiator,
            &mut trace,
        )
        .await
        .expect("escalate 不该出 DbError");
        match outcome {
            EscalationOutcome::Applied(escalation, approvers) => {
                assert_eq!(approvers.len(), 2, "上级角色 3 人 − 发起人 1 人 = 2");
                assert!(
                    !approvers.iter().any(|c| c.user_id == fx.initiator),
                    "发起人不能被塞回来"
                );
                assert_eq!(escalation.from_role_code, format!("P302UT_MAKER_a"));
                assert_eq!(escalation.reason, ESCALATE_REASON_SKIP);
                assert_eq!(escalation.levels.len(), 2, "中间空角色 1 级 + 命中 1 级");
                assert_eq!(escalation.levels[0].role_id, fx.mid_role);
                assert_eq!(escalation.levels[0].usable, 0, "中间级 0 人");
                assert_eq!(escalation.levels[1].usable, 2);
                assert!(escalation.applied);
            }
            other => panic!("期望 Applied，实际 {other:?}"),
        }
        teardown(&fx).await;
    }

    /// ② 链尾没有人 → `Exhausted`（引擎据此 422 并把链路带进报文）；
    ///    同时验证 `NO_MEMBER` 原因原样留在痕迹里。
    #[tokio::test]
    async fn escalate_reports_exhausted_with_chain() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("b").await;
        // 只有链尾角色（0 成员、无上级）
        exec(&format!(
            "UPDATE sys_role SET escalation_role_id = NULL WHERE id = '{0}'::uuid",
            fx.dead_role
        ))
        .await;
        let mut trace = Vec::new();
        let outcome = escalate(
            db(),
            &[fx.dead_role],
            ESCALATE_REASON_NO_MEMBER,
            false,
            fx.initiator,
            &mut trace,
        )
        .await
        .expect("escalate 不该出 DbError");
        match outcome {
            EscalationOutcome::Exhausted(escalation) => {
                assert!(!escalation.applied);
                assert!(escalation.levels.is_empty(), "没有上级角色，一级都没走");
                assert_eq!(escalation.reason, ESCALATE_REASON_NO_MEMBER);
                // 链路文案里至少有原角色名（报文/载荷都读它）
                let text = escalation.chain_text();
                assert!(text.contains("夹具-链尾"), "链路文案：{text}");
                assert_eq!(escalation.reason_label(), "角色无成员");
            }
            other => panic!("期望 Exhausted，实际 {other:?}"),
        }
        teardown(&fx).await;
    }

    /// ③ 成环 → `65040`（两级都 0 人，第 3 跳回到走过的角色）。
    #[tokio::test]
    async fn escalate_detects_cycle_with_65040() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("c").await;
        // 造环：dead → mid → dead（都是 0 人，保证走到环检测那一跳）
        exec(&format!(
            "UPDATE sys_role SET escalation_role_id = '{1}'::uuid WHERE id = '{0}'::uuid",
            fx.dead_role, fx.mid_role
        ))
        .await;
        exec(&format!(
            "UPDATE sys_role SET escalation_role_id = '{0}'::uuid WHERE id = '{1}'::uuid",
            fx.dead_role, fx.mid_role
        ))
        .await;
        let mut trace = Vec::new();
        let outcome = escalate(
            db(),
            &[fx.dead_role],
            ESCALATE_REASON_NO_MEMBER,
            false,
            fx.initiator,
            &mut trace,
        )
        .await
        .expect("escalate 不该出 DbError");
        match outcome {
            EscalationOutcome::Invalid(error) => {
                assert_eq!(error.code, error_code::FLOW_ESCALATION_INVALID);
                assert_eq!(error.code, 65040);
                assert!(error.msg.contains("成环"), "报文：{}", error.msg);
            }
            other => panic!("期望 Invalid(65040)，实际 {other:?}"),
        }
        teardown(&fx).await;
    }
}

// ==================== 兜底人：纯逻辑单测（P3-03） ====================

#[cfg(test)]
mod fallback_tests {
    use super::*;

    fn snapshot_node(assignee_type: &str) -> SnapshotNode {
        SnapshotNode {
            key: "n1".to_string(),
            name: "销售主管审批".to_string(),
            node_type: "APPROVAL".to_string(),
            assignee_type: Some(assignee_type.to_string()),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: "ERROR".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            config: Json::Object(Default::default()),
            assignees: Vec::new(),
        }
    }

    /// 报文必须能回答「降级链走到哪了」：两层都没配 / 只配了升级链 / 兜底人不可用 / 兜底人也被内控剔掉。
    #[test]
    fn dispatch_hint_names_every_missing_layer() {
        // ① 两层都没配 —— 卡面验收 ④ 钉的就是这句话（前端按它提示「去哪儿配」）
        assert_eq!(dispatch_hint(None, None), "升级链与兜底人均未配置");

        // ①b 「角色没有上级」时 escalate 也返回 Exhausted，但一级都没走 = 链没配
        let unconfigured = Escalation {
            reason: ESCALATE_REASON_SKIP.to_string(),
            from_role_code: "SALES_CLERK".to_string(),
            from_role_name: "销售员".to_string(),
            levels: Vec::new(),
            applied: false,
        };
        assert_eq!(
            dispatch_hint(Some(&unconfigured), None),
            "升级链与兜底人均未配置",
            "一级都没走 = 升级链没配，不能写成「升级链已走完」"
        );

        // ② 升级链走完仍无人 + 没配兜底
        let escalation = Escalation {
            reason: ESCALATE_REASON_SKIP.to_string(),
            from_role_code: "SALES_CLERK".to_string(),
            from_role_name: "销售员".to_string(),
            levels: vec![EscalationLevel {
                role_id: Uuid::from_u128(1),
                role_code: "SALES_SUPERVISOR".to_string(),
                role_name: "销售主管".to_string(),
                member_rows: 1,
                usable: 0,
            }],
            applied: false,
        };
        let text = dispatch_hint(Some(&escalation), None);
        assert!(text.contains("升级链已走完"), "{text}");
        assert!(text.contains("销售主管（0/1 人）"), "{text}");
        assert!(text.contains("兜底人未配置"), "{text}");

        // ③ 只配了兜底人但没派上
        let unavailable = Fallback {
            target: "张三".to_string(),
            by_role: false,
            applied: false,
            reason: "已停用".to_string(),
        };
        let text = dispatch_hint(None, Some(&unavailable));
        assert!(text.contains("兜底人「张三」不可用"), "{text}");
        assert!(text.contains("已停用"), "{text}");

        // ④ 兜底人派上了、但被引擎的内控 partition 又剔掉（配置错成制单人）
        let applied = Fallback {
            target: "李四".to_string(),
            by_role: false,
            applied: true,
            reason: "兜底审批人接管".to_string(),
        };
        let text = dispatch_hint(Some(&escalation), Some(&applied));
        assert!(text.contains("升级链已走完"), "{text}");
        assert!(text.contains("兜底人「李四」也已不可用"), "{text}");
    }

    /// 快照里的两列是 `String`：空串 = 没配；非法 UUID 也要能解析成 `None`
    /// （由 `apply_fallback` 据「原值非空」报 65042，不静默当成没配）。
    #[test]
    fn fallback_ids_parse_from_snapshot_strings() {
        let mut spec = snapshot_node("ROLE");
        assert_eq!(parse_fallback_ids(&spec), (None, None));

        let user = Uuid::from_u128(42);
        spec.fallback_user_id = Some(user.to_string());
        assert_eq!(parse_fallback_ids(&spec), (Some(user), None));

        let role = Uuid::from_u128(43);
        spec.fallback_user_id = Some(String::new());
        spec.fallback_role_id = Some(role.to_string());
        assert_eq!(parse_fallback_ids(&spec), (None, Some(role)));

        spec.fallback_user_id = Some("not-a-uuid".to_string());
        spec.fallback_role_id = None;
        assert_eq!(parse_fallback_ids(&spec), (None, None));
        assert!(
            spec.fallback_user_id.is_some(),
            "原值仍在 —— 运行期据此报 65042 而不是当成没配"
        );
    }
}

// ==================== 兜底人：真库单测（P3-03） ====================
//
// 覆盖卡面验收 ①②③④ 的**运行期**语义（纯 `resolve`，不起 HTTP）：
//   ① 升级链走完仍空 + 配 fallback_user → 该人收到待办且 assigned_by_rule=FALLBACK
//   ② 兜底人 = 发起人 → 422（65018，报文说「内控不允许自审」）
//   ③ 兜底角色 3 人 → 3 个审批人（ALL/ANY 由 P3-06 决定，本卡按现行为 = 全部落行）
//   ④ 未配兜底 → 仍 65018 且报文含「升级链与兜底人均未配置」
//   ⑤⑥ 兜底人不存在 / 已停用 → 65042（配错要吵）
//
// 与 `escalation_db_tests` 一样靠 `ERP_DB_URL` 开关：不设就静默 SKIP（CI 无库时不假绿）。
#[cfg(test)]
mod fallback_db_tests {
    use super::*;
    use crate::business::flow::graph::SnapshotAssignee;

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
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 一轮夹具（P303UT_* 前缀，结束即清理）：
    /// - `maker_role`：**只有发起人**（SKIP 剔光）且**没有上级** → 升级链走完仍 0 人；
    /// - `fb_role`：3 个在职成员（都不是发起人）；
    /// - `stopped`：停用用户（兜底人配成他 = 65042）。
    struct Fixture {
        maker_role: Uuid,
        fb_role: Uuid,
        initiator: Uuid,
        fb1: Uuid,
        fb2: Uuid,
        fb3: Uuid,
        stopped: Uuid,
    }

    async fn setup(tag: &str) -> Fixture {
        for sql in [
            format!(
                "DELETE FROM sys_user_role WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'P303UT\\_%\\_{tag}')"
            ),
            format!("DELETE FROM sys_user WHERE username LIKE 'p303ut\\_%\\_{tag}'"),
            format!("DELETE FROM sys_role WHERE role_code LIKE 'P303UT\\_%\\_{tag}'"),
        ] {
            exec(&sql).await;
        }
        let maker_role = Uuid::now_v7();
        let fb_role = Uuid::now_v7();
        let initiator = Uuid::now_v7();
        let fb1 = Uuid::now_v7();
        let fb2 = Uuid::now_v7();
        let fb3 = Uuid::now_v7();
        let stopped = Uuid::now_v7();
        exec(&format!(
            "INSERT INTO sys_role (id, role_code, role_name, status, sort_order) VALUES \
             ('{maker_role}'::uuid, 'P303UT_MAKER_{tag}', 'P3-03 夹具-发起人角色', 1, 910), \
             ('{fb_role}'::uuid, 'P303UT_FB_{tag}', 'P3-03 夹具-兜底角色', 1, 911)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
             ('{initiator}'::uuid, 'p303ut_init_{tag}', 'x', 'P3-03 发起人', 1), \
             ('{fb1}'::uuid, 'p303ut_fb1_{tag}', 'x', 'P3-03 兜底甲', 1), \
             ('{fb2}'::uuid, 'p303ut_fb2_{tag}', 'x', 'P3-03 兜底乙', 1), \
             ('{fb3}'::uuid, 'p303ut_fb3_{tag}', 'x', 'P3-03 兜底丙', 1), \
             ('{stopped}'::uuid, 'p303ut_stop_{tag}', 'x', 'P3-03 已停用', 0)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES \
             ('{initiator}'::uuid, '{maker_role}'::uuid), \
             ('{fb1}'::uuid, '{fb_role}'::uuid), \
             ('{fb2}'::uuid, '{fb_role}'::uuid), \
             ('{fb3}'::uuid, '{fb_role}'::uuid)"
        ))
        .await;
        Fixture {
            maker_role,
            fb_role,
            initiator,
            fb1,
            fb2,
            fb3,
            stopped,
        }
    }

    async fn teardown(fx: &Fixture) {
        for sql in [
            format!(
                "DELETE FROM sys_user_role WHERE user_id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid,'{3}'::uuid,'{4}'::uuid)",
                fx.initiator, fx.fb1, fx.fb2, fx.fb3, fx.stopped
            ),
            format!(
                "DELETE FROM sys_user WHERE id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid,'{3}'::uuid,'{4}'::uuid)",
                fx.initiator, fx.fb1, fx.fb2, fx.fb3, fx.stopped
            ),
            format!(
                "UPDATE sys_role SET escalation_role_id = NULL WHERE id IN ('{0}'::uuid,'{1}'::uuid)",
                fx.maker_role, fx.fb_role
            ),
            format!(
                "DELETE FROM sys_role WHERE id IN ('{0}'::uuid,'{1}'::uuid)",
                fx.maker_role, fx.fb_role
            ),
        ] {
            exec(&sql).await;
        }
    }

    /// 节点：ROLE 来源 + 指定 maker 角色；`empty_assignee` 由用例决定。
    fn node_with(
        maker_role: Uuid,
        empty_assignee: &str,
        fallback_user: Option<Uuid>,
        fallback_role: Option<Uuid>,
    ) -> SnapshotNode {
        SnapshotNode {
            key: "n_fb".to_string(),
            name: "销售主管审批".to_string(),
            node_type: "APPROVAL".to_string(),
            assignee_type: Some("ROLE".to_string()),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: empty_assignee.to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: fallback_user.map(|id| id.to_string()),
            fallback_role_id: fallback_role.map(|id| id.to_string()),
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            config: Json::Object(Default::default()),
            assignees: vec![SnapshotAssignee {
                assignee_type: "ROLE".to_string(),
                ref_id: maker_role.to_string(),
                sort_order: 0,
            }],
        }
    }

    fn ctx_of(initiator: Uuid) -> InstanceCtx {
        InstanceCtx {
            instance_id: Uuid::now_v7(),
            initiator_id: initiator,
            initiator_dept_id: None,
            biz_type: "P303UT".to_string(),
            biz_id: None,
            form_data: serde_json::json!({}),
        }
    }

    /// ① 升级链走完仍空（角色只有发起人、无上级）+ 配 fallback_user
    ///    → 该人收到待办、`assigned_by_rule='FALLBACK'`。
    #[tokio::test]
    async fn fallback_user_takes_over_after_escalation_exhausted() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("a").await;
        let node = node_with(fx.maker_role, "ERROR", Some(fx.fb1), None);
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let finalized = resolution.finalized;
        assert!(
            finalized.error.is_none(),
            "兜底人接管后不该再有错误：{:?}",
            finalized.error.as_ref().map(|e| (&e.code, &e.msg))
        );
        assert_eq!(finalized.approvers.len(), 1, "兜底人 1 人");
        assert_eq!(finalized.approvers[0].user_id, fx.fb1);
        assert_eq!(
            finalized.assigned_by_rule.as_deref(),
            Some(ASSIGNED_BY_FALLBACK),
            "派单规则必须是 FALLBACK（引擎照它写 biz_flow_task.assigned_by_rule）"
        );
        let fallback = finalized.fallback.as_ref().expect("兜底痕迹不能丢");
        assert!(fallback.applied);
        assert!(!fallback.by_role);
        assert!(!finalized.auto_pass, "兜底成功 ≠ 自动通过");
        // 升级链确实走过（起点只有发起人 → SKIP 剔光 → 无上级 → Exhausted）
        let escalation = finalized.escalation.as_ref().expect("升级链痕迹");
        assert!(!escalation.applied);
        assert_eq!(escalation.reason, ESCALATE_REASON_SKIP);
        teardown(&fx).await;
    }

    /// ② 兜底人 = 发起人 → 422 / 65018，报文写清「兜底人即发起人，内控不允许自审」。
    #[tokio::test]
    async fn fallback_user_equal_to_initiator_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("b").await;
        let node = node_with(fx.maker_role, "ERROR", Some(fx.initiator), None);
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let error = resolution.finalized.error.expect("自审兜底必须失败");
        assert_eq!(error.code, error_code::FLOW_NO_ASSIGNEE);
        assert_eq!(error.code, 65018);
        assert!(error.msg.contains("内控不允许自审"), "报文：{}", error.msg);
        assert!(error.msg.contains("兜底人即发起人"), "报文：{}", error.msg);
        assert!(resolution.finalized.approvers.is_empty(), "不许放行");
        teardown(&fx).await;
    }

    /// ③ 兜底角色 3 人 → 3 行待办（ALL/ANY 由 P3-06 决定，本卡按现行为全落行）。
    #[tokio::test]
    async fn fallback_role_expands_all_active_members() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("c").await;
        let node = node_with(fx.maker_role, "ERROR", None, Some(fx.fb_role));
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let finalized = resolution.finalized;
        assert!(finalized.error.is_none(), "{:?}", finalized.error);
        let mut ids: Vec<Uuid> = finalized.approvers.iter().map(|c| c.user_id).collect();
        ids.sort();
        let mut want = vec![fx.fb1, fx.fb2, fx.fb3];
        want.sort();
        assert_eq!(ids, want, "兜底角色 3 个在职成员必须全部落行");
        assert_eq!(
            finalized.assigned_by_rule.as_deref(),
            Some(ASSIGNED_BY_FALLBACK)
        );
        assert!(
            finalized
                .fallback
                .as_ref()
                .is_some_and(|fb| fb.by_role && fb.applied)
        );
        teardown(&fx).await;
    }

    /// ④ 未配兜底 → 仍 422 / 65018，且报文点名「升级链与兜底人均未配置」。
    #[tokio::test]
    async fn without_fallback_still_65018_and_names_both_layers() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("d").await;
        let node = node_with(fx.maker_role, "ERROR", None, None);
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let error = resolution.finalized.error.expect("没人可派必须 422");
        assert_eq!(error.code, 65018);
        assert!(
            error.msg.contains("升级链与兜底人均未配置"),
            "报文必须点名两层都没配：{}",
            error.msg
        );
        // 结构化诊断仍要带（前端按它弹「指派审批人」引导）
        assert!(
            error.data.is_some(),
            "422 必须继续带 payload（no_assignee 结构化诊断）"
        );
        teardown(&fx).await;
    }

    /// ⑤ 兜底人 id 不存在 → 65042（配错要吵，不许静默当成「没配」）。
    #[tokio::test]
    async fn unknown_fallback_user_reports_65042() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("e").await;
        let ghost = Uuid::now_v7();
        let node = node_with(fx.maker_role, "ERROR", Some(ghost), None);
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let error = resolution.finalized.error.expect("兜底人不存在必须报错");
        assert_eq!(error.code, error_code::FLOW_FALLBACK_INVALID);
        assert_eq!(error.code, 65042);
        assert!(error.msg.contains("不存在"), "报文：{}", error.msg);
        teardown(&fx).await;
    }

    /// ⑥ 兜底人已停用 → 65042（报文说清「已停用」，不是含糊的「没有审批人」）。
    #[tokio::test]
    async fn stopped_fallback_user_reports_65042() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("f").await;
        let node = node_with(fx.maker_role, "ERROR", Some(fx.stopped), None);
        let resolution = resolve(db(), &node, &ctx_of(fx.initiator), &EvalContext::default())
            .await
            .expect("resolve 不该出 DbError");
        let error = resolution.finalized.error.expect("兜底人停用必须报错");
        assert_eq!(error.code, 65042);
        assert!(error.msg.contains("已停用"), "报文：{}", error.msg);
        teardown(&fx).await;
    }
}
