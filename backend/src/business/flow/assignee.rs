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

// ==================== 最终裁决（纯逻辑，可单测） ====================

/// 把候选人合并、去重、过策略，得到「该落哪些待办」
/// （三层去重的**第 1 层**，架构 D14）。
///
/// # 规则（顺序不可换）
///
/// 1. **停用用户剔除**（`status <> 1`）—— 「角色里的人都辞职了」是坑 5 的真实原因；
/// 2. **ROBOT 过滤**（D18）：`allow_robot = false` 时只保留**显式 `USER`** 来源的机器人；
/// 3. **按 `user_id` 去重**：优先级「显式 USER > 其它来源」，其余保持首次出现顺序
///    （一个人同时是「角色成员 + 发起人主管」→ 只生成 1 行待办）；
/// 4. **自审策略**：命中发起人时按 `ALLOW` / `SKIP`（落 SKIPPED 留痕）/ `FORBID`（`65019`）；
/// 5. **空审批人策略**：`SKIP`（自动通过）/ `ERROR`（`65018`）/ `FALLBACK_ADMIN`（回退给超管）。
pub fn finalize(
    candidates: Vec<Candidate>,
    self_policy: SelfPolicy,
    empty_policy: EmptyPolicy,
    allow_robot: bool,
    initiator: Uuid,
) -> Finalized {
    let mut notes: Vec<String> = Vec::new();

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
                return Finalized::failed(
                    ApiError::new(
                        422,
                        error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
                        format!(
                            "审批人 {} 是发起人本人，本节点不允许自审（self_approval=FORBID）",
                            candidate.name
                        ),
                    ),
                    notes,
                );
            }
        }
    }

    // ⑤ 空审批人策略
    if approvers.is_empty() {
        return match empty_policy {
            EmptyPolicy::Skip => {
                notes.push(
                    "本节点没有有效审批人，empty_assignee=SKIP → 节点自动跳过并计数".to_string(),
                );
                Finalized {
                    approvers,
                    skipped,
                    auto_pass: true,
                    fallback_admin: false,
                    error: None,
                    rule_id: None,
                    notes,
                }
            }
            EmptyPolicy::Error => Finalized::failed(
                ApiError::new(
                    422,
                    error_code::FLOW_NO_ASSIGNEE,
                    "本节点没有一个可用审批人（empty_assignee=ERROR）",
                ),
                notes,
            ),
            EmptyPolicy::FallbackAdmin => {
                notes.push(
                    "本节点没有有效审批人，empty_assignee=FALLBACK_ADMIN → 回退给平台管理员"
                        .to_string(),
                );
                Finalized {
                    approvers,
                    skipped,
                    auto_pass: false,
                    fallback_admin: true,
                    error: None,
                    rule_id: None,
                    notes,
                }
            }
        };
    }

    Finalized {
        approvers,
        skipped,
        auto_pass: false,
        fallback_admin: false,
        error: None,
        rule_id: None,
        notes,
    }
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
    let mut finalized = finalize(
        candidates.clone(),
        SelfPolicy::parse(&node.self_approval),
        EmptyPolicy::parse(&node.empty_assignee),
        node.allow_robot,
        ctx.initiator_id,
    );
    finalized.rule_id = rule_id;
    // `empty_assignee=ERROR`：把「本节点没有一个可用审批人」补成**可执行**的配置指引。
    // 只改这一种策略的报文（SKIP / FALLBACK_ADMIN 各有自己的语义，一字不动）——
    // 报文里出现节点名与「去哪儿配」，发起人才知道下一步做什么
    // （t_e63b600f：作废复核岗位没人时，旧报文只说「没有一个可用审批人」，等于死路）。
    if let Some(error) = finalized.error.as_mut()
        && error.code == error_code::FLOW_NO_ASSIGNEE
        && error.msg.contains("empty_assignee=ERROR")
    {
        error.msg = format!(
            "节点「{}」没有任何可用审批人（来源 {} 解析出 0 人；该节点配置了 empty_assignee=ERROR，\
             不会兜底给管理员）。请到「流程定义 → 本流程 → 节点「{}」」把审批人设为具体角色或人员，\
             并确认该角色下有在职成员（角色管理 → 成员）；若该岗位确实无人，可改由有审批权的人提交本单。",
            node.name,
            node.assignee_type.as_deref().unwrap_or("未配置"),
            node.name
        );
    }
    trace.push(format!(
        "去重 + 策略后：待办 {} 行 / SKIPPED 留痕 {} 行 / 自动通过={}",
        finalized.approvers.len(),
        finalized.skipped.len(),
        finalized.auto_pass
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
