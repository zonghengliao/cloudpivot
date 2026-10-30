//! 审批流 · 运行侧引擎（批次 D · D2）。
//!
//! 依据：卡面 t_3715c446「3. `flow/engine.rs`」+
//! `docs/批次D-工作流审批-技术架构.md` §4.1（实例状态机）/ §4.2（节点状态机）/
//! §4.4（一次流转的完整算法）/ §4.5（并发三件套）/ §4.6（退回 + round）/ §4.7（撤回）/
//! §4.8（加签转办）/ §13.1（坑 1 会签并发）/ §13.4（坑 4 退回回滚）。
//!
//! # 实例状态机（5 态 8 迁移，§4.1）
//!
//! ```text
//!             ┌──────────── 引擎：最后一个 END 通过 ─────────► APPROVED
//!             ├──────────── 引擎：驳回且 return_to=NONE ─────► REJECTED
//! (无) → PENDING ─────────── 发起人本人 + 零人已审 ──────────► WITHDRAWN
//!             └──────────── 管理员 / 业务侧终止 ─────────────► CANCELLED
//! ```
//!
//! 终态**永不回** `PENDING`（要重来就新发起 + `prev_instance_id` 串联，架构 §4.1）。
//! [`transition_allowed`] 把这张表写成纯函数并有 5×5 矩阵单测（验收 25）。
//!
//! # 节点状态机（6 态，§4.2）
//!
//! `PENDING`（等人齐）/ `ACTIVE`（审批中）/ `APPROVED` / `REJECTED` / `SKIPPED`（通过空审批人、
//! 自审跳过、`CC` 节点）/ `CANCELLED`（被退回掐断；同一行不复活，重进是 `round + 1` 的新行）。
//!
//! # 🔴 并发四件事（D15，架构 §4.5.1）
//!
//! 1. **锁顺序固定**：`biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance`。
//!    所有写路径都按这个顺序取锁，否则「同意」与「退回」并发会 ABBA 死锁。
//! 2. **`FOR UPDATE` 包住「计票 + 判定 + 流转」**：票数一律 `COUNT(*)` 现算，
//!    **本文件没有任何票数自增的写法**（[`tests::no_accumulator_on_vote_counts`] 用静态扫描钉住）。
//! 3. **23505 当幂等成功**：重复落待办 / 重复激活节点都走「幂等成功」分支，
//!    **不回 500**（[`model::is_pending_duplicate`] / [`model::is_node_duplicate`]）。
//! 4. **副作用锁外发送**：锁内只 `effects.push(...)` 收集，事务提交后由 handler 调
//!    [`flush_effects`]（发通知失败不影响业务，只记 WARN）。
//!
//! # 业务错误为什么不抛 `Err`
//!
//! `db().transaction(closure)` 的闭包只能返回 [`DbError`]。若把「这条待办不是你的」
//! 这种业务错误塞进 `DbError`，错误码（`65003` / `65028` / `65023`）就丢了，
//! 前端只能看到 `500 + 10004`。所以引擎把业务拒绝放进 [`ActionOut::rejected`]：
//! 事务正常提交（幂等分支 / 无写入时等于什么都没做），handler 再把 `rejected` 转成
//! 对应的 `ApiError`。**真正的数据库故障仍然走 `Err(DbError)` → 整单回滚。**
//!
//! # 一次流转（§4.4）
//!
//! ```text
//! 锁待办 → 锁节点 → 锁实例
//!   → 写这一票 → COUNT(*) 现算 → 覆写展示冗余（绝对值）
//!   → judge(mode, ratio, veto, tally)
//!        Wait   → 什么都不做（节点保持 ACTIVE）
//!        Pass   → 节点 APPROVED + 其余 PENDING 待办 CANCELLED + 驱动下游
//!        Reject → 节点 REJECTED + apply_return（退回 = 同实例内 round + 1）
//!   → 实例 CAS（revision + 1），0 行则重读重试一次，仍失败 → 409
//! ```
//!
//! 下游驱动用**显式队列**（[`Run::drive`]）而不是递归 `async fn`：
//! 递归 async fn 在 Rust 里要 `Box::pin` 才能编译，而显式队列顺带让「并行分支」
//! 天然按优先级逐条推进、日志可读。

use std::collections::VecDeque;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::assignee::{self, Resolution};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use serde_json::Map;
use serde_json::Value as Json;

use crate::business::flow::graph::{
    self, ApprovalMode, EvalContext, GatewayMode, NodeType, Snapshot, SnapshotEdge, SnapshotNode,
};
use crate::business::flow::model::{self, Tally};
use crate::shared::error_code;

/// 单实例最大轮次（防止退回打转把库写爆；超了直接拒绝并提示人工介入）。
pub const MAX_ROUND: i32 = 50;

/// 驳回意见最小字数（卡面「驳回（必填意见 ≥5 字）」）。
pub const REJECT_COMMENT_MIN_CHARS: usize = 5;

/// 单次加签最多人数（架构 §4.8.2 的 `≤ 10`）。
pub const ADD_SIGN_MAX_USERS: usize = 10;

/// 单次批量审批最多条数（架构 §10.1.3 #28 的 `≤ 50`）。
pub const BATCH_MAX_TASKS: usize = 50;

// ==================== 判定（纯函数） ====================

/// 节点判定结果（架构 §3.3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 通过（继续流转）。
    Pass,
    /// 驳回（立即短路：其余待办 `CANCELLED`）。
    Reject,
    /// 等票。
    Wait,
    /// 没有有效审批人（节点 `SKIPPED` + `auto_passed_nodes + 1`）。
    AutoPass,
}

/// 会签判定（**纯函数**，架构 §3.3.2 的 9 条逐条落地）。
///
/// 顺序不可换：`veto` 最先短路（一票否决与 mode 正交，D4），
/// `RATIO` 的「追不上阈值提前判死」在 `Pass` 之后。
///
/// # 参数
///
/// - `mode`：`ALL` / `RATIO` / `ANY`（单签节点按 `ALL` 处理，`n = 1` 时等价于或签）；
/// - `ratio`：只有 `RATIO` 用，`None` 视为 `1.0`；
/// - `veto`：`true` 时任一反对立即驳回；
/// - `tally`：`COUNT(*)` 现算出来的票数（`total` 不含 `SKIPPED` / `TRANSFERRED` / `CANCELLED`）。
pub fn judge(mode: ApprovalMode, ratio: Option<f64>, veto: bool, tally: Tally) -> Verdict {
    // ⑨ n == 0：全部审批人被跳过（自审 SKIP / 空审批人）→ 自动通过
    if tally.total <= 0 {
        return Verdict::AutoPass;
    }
    // ① veto 且 r >= 1 → Reject（短路，其余 PENDING 待办由调用方 CANCELLED）
    if veto && tally.rejected >= 1 {
        return Verdict::Reject;
    }
    let approved = tally.approved as f64;
    let total = tally.total as f64;
    // 阈值比较留一个极小容差：`3/5` 与字面量 `0.6` 在二进制浮点里可能差 1 ulp，
    // 而业务语义是「达到即通过」（验收 8 的 RATIO=0.6 五人三票必须提前通过）。
    const EPS: f64 = 1e-9;
    let threshold = ratio.unwrap_or(1.0).clamp(0.0, 1.0);

    // ② ④ ⑥ 通过条件
    let passed = match mode {
        ApprovalMode::All => tally.approved == tally.total,
        ApprovalMode::Ratio => approved / total + EPS >= threshold,
        ApprovalMode::Any => tally.approved >= 1,
    };
    if passed {
        return Verdict::Pass;
    }

    // ③ ⑤ ⑦ 判死条件（**所有模式共用兜底 `pending == 0`**：
    // 「都投完了但没通过」不该永远挂着 —— 这是架构 §3.3.2 的第 7 条）
    let hopeless = match mode {
        ApprovalMode::All => tally.pending == 0,
        ApprovalMode::Ratio => {
            // ⑤ 「本轮还能达到的最大通过率」也追不上阈值 → 提前判死，不必等他们把票点完。
            // 最大通过率 = (已同意 + 未表态) / 应到 —— **不是** (应到 - 已同意) / 应到：
            // 后者把「已驳回」也算成「还能同意」，RATIO(0.8) 五人 2 同意 3 待审时
            // 会算出 (5-2)/5 = 0.6 < 0.8 → 误判驳回（实测踩到，架构 §3.3.2 的公式只在 r=0 时成立）。
            (approved + tally.pending as f64) / total + EPS < threshold || tally.pending == 0
        }
        ApprovalMode::Any => tally.pending == 0,
    };
    if hopeless {
        Verdict::Reject
    } else {
        Verdict::Wait
    }
}

/// 实例状态迁移是否合法（5×5 矩阵，架构 §4.1）。
///
/// 只允许 `PENDING → {APPROVED, REJECTED, CANCELLED, WITHDRAWN}` 与「原地不动」；
/// 终态之间互转、终态回 `PENDING` 全部 `false`。`same_as_self` 单独用 [`instance_transition`]。
pub fn transition_allowed(from: &str, to: &str) -> bool {
    match (from, to) {
        ("PENDING", "APPROVED")
        | ("PENDING", "REJECTED")
        | ("PENDING", "CANCELLED")
        | ("PENDING", "WITHDRAWN") => true,
        // 幂等：同一个状态再写一次不算迁移（重复提交 / 重试）
        (a, b) if a == b => true,
        _ => false,
    }
}

/// 把「引擎要做的动作」翻译成实例迁移（纯函数，单测用）。
pub fn instance_transition(action: InstanceAction, from: &str) -> Option<&'static str> {
    let to = match action {
        InstanceAction::Start => "PENDING",
        InstanceAction::Finish => "APPROVED",
        InstanceAction::RejectFinal => "REJECTED",
        InstanceAction::Withdraw => "WITHDRAWN",
        InstanceAction::Cancel => "CANCELLED",
    };
    if transition_allowed(from, to) {
        Some(to)
    } else {
        None
    }
}

/// 引擎动作（→ 实例迁移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceAction {
    /// 发起。
    Start,
    /// 走到 END。
    Finish,
    /// 驳回并终止（`return_to = NONE`）。
    RejectFinal,
    /// 发起人撤回。
    Withdraw,
    /// 管理员 / 业务侧终止。
    Cancel,
}

// ==================== 副作用（锁外发送） ====================

/// 一条待发通知。
#[derive(Debug, Clone)]
pub struct Notice {
    /// 收件人（去重后可能为空 → 不发）。
    pub recipients: Vec<Uuid>,
    /// 标题。
    pub title: String,
    /// 正文。
    pub content: String,
    /// 跳转地址（前端路由）。
    pub link_url: String,
    /// 业务单 id（通知中心按它聚合）。
    pub instance_id: Uuid,
    /// 是否强制送达（绕过偏好；`SKIP` / 空审批人的告警用强制）。
    pub force: bool,
}

/// 锁内收集的副作用集合。
#[derive(Debug, Clone, Default)]
pub struct Effects {
    /// 待发通知。
    pub notices: Vec<Notice>,
}

impl Effects {
    /// 收集一条通知（收件人排序去重、去空）。
    pub fn notice(
        &mut self,
        recipients: Vec<Uuid>,
        title: impl Into<String>,
        content: impl Into<String>,
        link_url: impl Into<String>,
        instance_id: Uuid,
        force: bool,
    ) {
        let mut recipients = recipients;
        recipients.sort();
        recipients.dedup();
        if recipients.is_empty() {
            return;
        }
        self.notices.push(Notice {
            recipients,
            title: title.into(),
            content: content.into(),
            link_url: link_url.into(),
            instance_id,
            force,
        });
    }

    /// 取出全部通知（handler 提交后逐条发送）。
    pub fn take(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }
}

/// 发送锁外副作用（**必须在事务提交之后调用**）。
///
/// 通知失败只记 WARN，不影响业务（架构 §4.11 时序 1 的「事务边界」注）。
///
/// # 返回值
///
/// 实际发出的通知条数（发送失败的那条不算）。
pub async fn flush_effects(effects: Vec<Notice>) -> usize {
    let mut sent = 0usize;
    for notice in effects {
        let Notice {
            recipients,
            title,
            content,
            link_url,
            instance_id,
            force,
        } = notice;
        let mut draft = NoticeDraft::text(
            NoticeType::Todo,
            NoticeLevel::Info,
            title,
            content,
            NoticeTarget::User(recipients),
        );
        draft.link_url = Some(link_url);
        draft.biz_type = Some("flow".to_string());
        draft.biz_id = Some(instance_id.to_string());
        // `force` 只管把是否绕过偏好传下去：偏好的唯一实现在 notice::dispatch（T2 的教训）
        draft.force = force;
        match notice::dispatch(draft).await {
            Ok(result) => {
                sent += 1;
                tracing::info!(
                    notice_id = %result.notice_id,
                    receivers = result.receiver_count,
                    "审批流通知已发送"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "审批流通知发送失败（不影响业务）");
            }
        }
    }
    sent
}

// ==================== 引擎出参 ====================

/// 一次引擎动作的结果（handler 直接转 DTO）。
///
/// **不 derive `Clone`**：`rejected: Option<ApiError>` 而 `ApiError` 没有实现 `Clone`。
/// 结果本来就只有「一个消费者」（handler 转 DTO 或转 ApiError），不需要克隆。
#[derive(Debug, Default)]
pub struct ActionOut {
    /// 实例 id。
    pub instance_id: Option<Uuid>,
    /// 实例状态（动作后的）。
    pub instance_status: String,
    /// 当前节点状态（投票类动作）。
    pub node_status: Option<String>,
    /// 待办是否已处理完（幂等重复提交也为 `true`）。
    pub done: bool,
    /// 是否幂等命中（重复提交 / 23505 兜底）。
    pub idempotent: bool,
    /// 本次激活的下游节点 key。
    pub next_nodes: Vec<String>,
    /// 自动通过节点数（派生值）。
    pub auto_passed_nodes: i32,
    /// 应到人数。
    pub approver_count: i32,
    /// 已同意 / 已驳回（`COUNT(*)` 现算）。
    pub approved_count: i32,
    /// 已驳回。
    pub rejected_count: i32,
    /// 本次动作触发的通知（**事务提交后**再发）。
    pub effects: Effects,
    /// 业务拒绝（handler 转 `ApiError`）。
    pub rejected: Option<ApiError>,
}

impl ActionOut {
    /// 业务拒绝。
    pub fn deny(instance_id: Option<Uuid>, error: ApiError) -> Self {
        Self {
            instance_id,
            instance_status: String::new(),
            rejected: Some(error),
            ..ActionOut::default()
        }
    }

    /// 是否成功（无业务拒绝）。
    pub fn ok(&self) -> bool {
        self.rejected.is_none()
    }
}

// ==================== 发起 ====================

/// 发起入参（handler 已做完字段校验与流程状态校验）。
#[derive(Debug, Clone)]
pub struct StartReq {
    /// 新实例 id。
    pub instance_id: Uuid,
    /// 流程定义。
    pub def_id: Uuid,
    /// 绑定版本。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 标题。
    pub title: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起人部门。
    pub initiator_dept_id: Option<Uuid>,
    /// 表单数据（发起时冻结，条件表达式的作用域）。
    pub form_data: Json,
}

/// 发起一张审批单（**事务内**，`ex` 传 `&tx`）。
///
/// 流程：建实例 → 读 `version.graph_snapshot`（**不 JOIN 定义行**，D2）→
/// 激活 `START` 节点（`START` 无审批人，直接 `APPROVED`）→ 驱动下游 →
/// `auto_passed_nodes` 派生 → 实例 CAS。
///
/// # 异常情况
///
/// - 版本未发布 / 快照缺失 / 快照解析失败 → `Err(DbError::InvalidValue)`（整单回滚）；
/// - `uk_biz_flow_instance_biz_active` 冲突 → `Err(DbError::UniqueViolation)`（handler 转 409）。
pub async fn start(ex: &Transaction, req: &StartReq) -> DbResult<ActionOut> {
    let snapshot = load_snapshot(ex, req.version_id).await?;
    let start_keys = start_keys_of(&snapshot);

    model::create_instance(
        ex,
        &model::NewInstance {
            id: req.instance_id,
            instance_no: model::new_instance_no(chrono::Utc::now()),
            def_id: req.def_id,
            version_id: req.version_id,
            version_no: req.version_no,
            flow_code: req.flow_code.clone(),
            flow_name: req.flow_name.clone(),
            biz_type: req.biz_type.clone(),
            biz_id: req.biz_id,
            biz_no: req.biz_no.clone(),
            title: req.title.clone(),
            initiator_id: req.initiator_id,
            initiator_name: req.initiator_name.clone(),
            initiator_dept_id: req.initiator_dept_id,
            form_data: req.form_data.clone(),
            due_at: None,
        },
    )
    .await?;

    let instance = model::lock_instance(ex, req.instance_id)
        .await?
        .ok_or(DbError::NotFound)?;

    let mut run = Run::new(ex, instance.clone(), snapshot);
    run.round = 1;
    let failure = run.drive(start_keys).await?;

    let auto_passed = model::sync_auto_passed(ex, instance.id).await?;
    let final_status = match (&failure, &run.final_status) {
        (None, Some(status)) => status.clone(),
        _ => "PENDING".to_string(),
    };
    // CAS：发起时 revision = 0 → 1（并发双击的第二个请求会拿到 0 行 → handler 报 409，
    // 库层还有 uk_biz_flow_instance_biz_active 兜底）
    let updated = model::bump_instance(
        ex,
        instance.id,
        instance.revision,
        Some(&final_status),
        None,
        run.round,
    )
    .await?;
    if updated == 0 {
        return Err(DbError::InvalidValue(format!(
            "审批单 {} 并发冲突（revision CAS 失败）",
            instance.id
        )));
    }

    let mut out = ActionOut {
        instance_id: Some(instance.id),
        instance_status: final_status,
        auto_passed_nodes: auto_passed,
        next_nodes: run.next_nodes.clone(),
        effects: run.effects,
        ..ActionOut::default()
    };
    if let Some(error) = failure {
        out.rejected = Some(error);
    }
    Ok(out)
}

// ==================== 投票：同意 ====================

/// 同意（**事务内**）。
///
/// 并发语义（架构 §4.5）：
/// - 锁 `biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance`；
/// - 票数 `COUNT(*)` 现算；
/// - 同一条待办被点两次 → `idempotent = true`（不是 500，也不是 409）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（整单回滚）。
pub async fn approve(
    ex: &Transaction,
    task_id: Uuid,
    operator: Uuid,
    comment: Option<&str>,
) -> DbResult<ActionOut> {
    vote(
        ex,
        task_id,
        operator,
        Vote::Approve,
        comment,
        ReturnTo::Prev,
        false,
    )
    .await
}

/// 驳回（**事务内**）。
///
/// `return_to` 决定退回目标（架构 §4.6.1）：
/// `Prev`（默认）/ `Initiator`（生成 `RESUBMIT` 待办）/ `Node(key)`（仅管理员）/
/// `Final`（驳回并终止 → 实例 `REJECTED`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn reject(
    ex: &Transaction,
    task_id: Uuid,
    operator: Uuid,
    comment: &str,
    return_to: ReturnTo,
) -> DbResult<ActionOut> {
    vote(
        ex,
        task_id,
        operator,
        Vote::Reject,
        Some(comment),
        return_to,
        false,
    )
    .await
}

/// 退回目标（架构 §4.6.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnTo {
    /// 上一节点（本轮已通过的上游里最近的那一个；没有则回到 `START`）。
    Prev,
    /// 发起人（生成 `task_kind = RESUBMIT` 待办）。
    Initiator,
    /// 指定节点（仅 `business:flow:admin`）。
    Node(String),
    /// 驳回并终止（实例 → `REJECTED`）。
    Final,
}

impl ReturnTo {
    /// 解析请求里的 `return_to`（`PREV` / `INITIATOR` / `NONE` / 节点 key）。
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        match trimmed.to_ascii_uppercase().as_str() {
            "" | "PREV" => ReturnTo::Prev,
            "INITIATOR" => ReturnTo::Initiator,
            "NONE" | "FINAL" => ReturnTo::Final,
            _ => ReturnTo::Node(trimmed.to_string()),
        }
    }

    /// 是否需要管理员权限（`business:flow:admin`）。
    pub fn needs_admin(&self) -> bool {
        matches!(self, ReturnTo::Node(_) | ReturnTo::Final)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vote {
    Approve,
    Reject,
}

async fn vote(
    ex: &Transaction,
    task_id: Uuid,
    operator: Uuid,
    vote: Vote,
    comment: Option<&str>,
    return_to: ReturnTo,
    is_auto: bool,
) -> DbResult<ActionOut> {
    // ① 锁待办行（锁序第一）
    let Some(task) = model::lock_task(ex, task_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_TASK_NOT_FOUND, "待办不存在或已被删除"),
        ));
    };
    if task.assignee_id != operator {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                403,
                error_code::FLOW_TASK_NOT_MINE,
                "这条待办不是你的，不能审批",
            ),
        ));
    }
    if !task.is_pending() {
        // 幂等：重复提交 / 已被别人处理 / 已转办 → 当成功返回，不报错（架构 §4.5.1 第 ① 步）
        let instance = model::find_instance(task.instance_id).await?;
        return Ok(ActionOut {
            instance_id: Some(task.instance_id),
            instance_status: instance
                .as_ref()
                .map(|row| row.status.clone())
                .unwrap_or_default(),
            node_status: Some(task.status.clone()),
            done: true,
            idempotent: true,
            auto_passed_nodes: instance
                .as_ref()
                .map(|row| row.auto_passed_nodes)
                .unwrap_or(0),
            ..ActionOut::default()
        });
    }

    // ② 锁节点行（锁序第二）
    let Some(node) = model::lock_node(ex, task.instance_node_id).await? else {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                404,
                error_code::FLOW_TASK_NOT_FOUND,
                "待办对应的节点行不存在",
            ),
        ));
    };
    if node.is_terminal() {
        let instance = model::find_instance(task.instance_id).await?;
        return Ok(ActionOut {
            instance_id: Some(task.instance_id),
            instance_status: instance
                .as_ref()
                .map(|row| row.status.clone())
                .unwrap_or_default(),
            node_status: Some(node.status.clone()),
            done: true,
            idempotent: true,
            ..ActionOut::default()
        });
    }

    // ③ 锁实例行（锁序第三）
    let Some(mut instance) = model::lock_instance(ex, task.instance_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_INSTANCE_NOT_FOUND, "审批单不存在"),
        ));
    };
    if !instance.is_pending() {
        return Ok(ActionOut::deny(
            Some(instance.id),
            ApiError::new(
                409,
                error_code::FLOW_INSTANCE_NOT_PENDING,
                format!("审批单已结束（{}），不能再审批", instance.status),
            ),
        ));
    }
    // 退回「任意节点 / 终止」是管理动作，权限在 handler 里用 `business:flow:admin` 现查；
    // 这里只做一次兜底断言，避免别的调用方绕过 handler 直接调引擎。
    if vote == Vote::Reject && return_to.needs_admin() && !matches!(return_to, ReturnTo::Final) {
        tracing::info!(
            %task_id,
            target = ?return_to,
            "退回指定节点：调用方应已在 handler 校验 business:flow:admin"
        );
    }

    // ④ 写这一票
    let status = if vote == Vote::Approve {
        "APPROVED"
    } else {
        "REJECTED"
    };
    model::finish_task(ex, task.id, status, comment, is_auto).await?;

    // ⑤ COUNT(*) 现算（**不信任任何累加器**，D15 第 3 层）
    let tally = model::tally_of_node(ex, node.id, node.round).await?;
    model::update_node_counts(ex, node.id, tally).await?;

    let mut run = Run::new(
        ex,
        instance.clone(),
        load_snapshot(ex, instance.version_id).await?,
    );
    run.round = task.round;
    run.approver_count = tally.total as i32;
    run.approved_count = tally.approved as i32;
    run.rejected_count = tally.rejected as i32;

    let mut out = ActionOut {
        instance_id: Some(instance.id),
        node_status: Some(node.status.clone()),
        done: true,
        approver_count: tally.total as i32,
        approved_count: tally.approved as i32,
        rejected_count: tally.rejected as i32,
        ..ActionOut::default()
    };

    match vote {
        Vote::Approve => {
            // `RESUBMIT` 待办（退回发起人后的重新提交）与 `START` 节点没有「票」的概念：
            // 直接结束节点并把流程往下推（§4.11 时序 3）。
            let node_type = NodeType::parse(&node.node_type).unwrap_or(NodeType::Approval);
            let is_resubmit = task.task_kind == "RESUBMIT" || node_type == NodeType::Start;
            let verdict = if is_resubmit {
                Verdict::Pass
            } else {
                judge(
                    ApprovalMode::parse(node.approval_mode.as_deref().unwrap_or("ALL"))
                        .unwrap_or(ApprovalMode::All),
                    node.approve_ratio,
                    node.reject_veto.unwrap_or(true),
                    tally,
                )
            };
            match verdict {
                Verdict::Wait => {
                    out.next_nodes = Vec::new();
                }
                Verdict::Pass => {
                    // 其余待办作废（会签提前通过 / 或签）—— 这是「不需要他们投了」，不是驳回
                    model::cancel_pending_of_node(
                        ex,
                        node.id,
                        Some(task.id),
                        "该节点已满足通过条件，无需再投",
                    )
                    .await?;
                    let passed = model::tally_of_node(ex, node.id, node.round).await?;
                    model::end_node_row(ex, node.id, "APPROVED", passed, None, false).await?;
                    out.node_status = Some("APPROVED".to_string());
                    let targets = run.out_targets(&node.node_key, None, false);
                    let failure = run.drive(targets).await?;
                    out.next_nodes = run.next_nodes.clone();
                    out.effects = run.effects;
                    if let Some(error) = failure {
                        out.rejected = Some(error);
                    }
                }
                Verdict::Reject => {
                    // 只可能出现在「本次投的是驳回票」的分支里，这里防御性处理成终止
                    model::end_node_row(ex, node.id, "REJECTED", tally, None, false).await?;
                    out.node_status = Some("REJECTED".to_string());
                    run.final_status = Some("REJECTED".to_string());
                }
                Verdict::AutoPass => {
                    model::end_node_row(ex, node.id, "SKIPPED", tally, None, true).await?;
                    out.node_status = Some("SKIPPED".to_string());
                    let targets = run.out_targets(&node.node_key, None, false);
                    let failure = run.drive(targets).await?;
                    out.next_nodes = run.next_nodes.clone();
                    out.effects = run.effects;
                    if let Some(error) = failure {
                        out.rejected = Some(error);
                    }
                }
            }
        }
        Vote::Reject => {
            // 驳回 = 当前节点 REJECTED + 其余待办 CANCELLED + 按目标退回（§4.6）
            model::end_node_row(ex, node.id, "REJECTED", tally, None, false).await?;
            out.node_status = Some("REJECTED".to_string());
            model::cancel_pending_of_node(ex, node.id, Some(task.id), "节点已被驳回").await?;
            let failure = run
                .apply_return(&node, &return_to, comment.unwrap_or_default())
                .await?;
            out.next_nodes = run.next_nodes.clone();
            out.effects = run.effects;
            if let Some(error) = failure {
                out.rejected = Some(error);
            }
        }
    }

    // ⑥ 实例 CAS（`revision` 自增）；0 行 → 重读重试一次 → 仍失败返回业务拒绝
    let auto_passed = model::sync_auto_passed(ex, instance.id).await?;
    out.auto_passed_nodes = auto_passed;
    let status_after = run
        .final_status
        .clone()
        .unwrap_or_else(|| "PENDING".to_string());
    let round_total = model::max_round(ex, instance.id)
        .await?
        .max(instance.round_total);
    let mut updated = model::bump_instance(
        ex,
        instance.id,
        instance.revision,
        Some(&status_after),
        None,
        round_total,
    )
    .await?;
    if updated == 0 {
        let refreshed = model::lock_instance(ex, instance.id).await?;
        if let Some(fresh) = refreshed {
            instance.revision = fresh.revision;
            instance.round_total = fresh.round_total;
            updated = model::bump_instance(
                ex,
                instance.id,
                instance.revision,
                Some(&status_after),
                None,
                round_total,
            )
            .await?;
        }
    }
    if updated == 0 {
        out.rejected = Some(ApiError::new(
            409,
            error_code::FLOW_INSTANCE_NOT_PENDING,
            "审批单正在被其他操作推进（revision CAS 冲突），请刷新后重试",
        ));
        return Ok(out);
    }
    out.instance_status = status_after;
    Ok(out)
}

/// 系统代点（超时自动通过 / 自动驳回，架构 §4.9.1 / D10）。
///
/// **全局闸在调用方**：`AUTO_APPROVE` 必须先确认 `biz_flow_def.allow_auto_approve = true`，
/// 否则降级成提醒（`timeout.rs` 负责这道闸）。本函数只保证「这一票记成系统代点」：
/// `is_auto = true` + 意见写明原因，审计能查出来是谁替人做的决定。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn auto_decide(
    ex: &Transaction,
    task_id: Uuid,
    approve: bool,
    comment: &str,
) -> DbResult<ActionOut> {
    let Some(task) = model::find_task(task_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_TASK_NOT_FOUND, "待办不存在"),
        ));
    };
    // 系统身份 = 这条待办的审批人本人（行级归属校验自然通过，审计里 `is_auto` 标注「系统代点」）
    let operator = task.assignee_id;
    vote(
        ex,
        task_id,
        operator,
        if approve { Vote::Approve } else { Vote::Reject },
        Some(comment),
        ReturnTo::Prev,
        true,
    )
    .await
}

// ==================== 撤回 ====================

/// 撤回（**事务内**）：只允许发起人本人且**零人已审**（架构 D9 / §4.7）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn withdraw(ex: &Transaction, instance_id: Uuid, operator: Uuid) -> DbResult<ActionOut> {
    let Some(instance) = model::lock_instance(ex, instance_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_INSTANCE_NOT_FOUND, "审批单不存在"),
        ));
    };
    if instance.initiator_id != operator {
        return Ok(ActionOut::deny(
            Some(instance.id),
            ApiError::new(
                403,
                error_code::FLOW_FORBIDDEN,
                "只有发起人本人可以撤回这张审批单",
            ),
        ));
    }
    if !instance.is_pending() {
        return Ok(ActionOut::deny(
            Some(instance.id),
            ApiError::new(
                409,
                error_code::FLOW_INSTANCE_NOT_PENDING,
                format!("审批单已结束（{}），不能撤回", instance.status),
            ),
        ));
    }
    // 「零人已审」必须精确：不算 SKIPPED（没审过）与 TRANSFERRED（转办出去的人也没审）
    let reviewed = model::reviewed_count(ex, instance.id).await?;
    if reviewed > 0 {
        return Ok(ActionOut::deny(
            Some(instance.id),
            ApiError::new(
                409,
                error_code::FLOW_INSTANCE_NOT_WITHDRAWABLE,
                format!("已有人审批（{reviewed} 票），不能撤回；请联系流程管理员终止"),
            ),
        ));
    }

    let pending = model::cancel_pending_of_instance(ex, instance.id, "发起人已撤回").await?;
    let cancelled_nodes = model::cancel_active_nodes(ex, instance.id, None).await?;
    let updated = model::bump_instance(
        ex,
        instance.id,
        instance.revision,
        Some("WITHDRAWN"),
        Some("发起人撤回"),
        instance.round_total,
    )
    .await?;
    if updated == 0 {
        return Ok(ActionOut::deny(
            Some(instance.id),
            ApiError::new(
                409,
                error_code::FLOW_INSTANCE_NOT_WITHDRAWABLE,
                "审批单正在被其他操作推进，请刷新后重试",
            ),
        ));
    }

    let mut effects = Effects::default();
    let recipients = model::pending_assignees(ex, instance.id).await?;
    effects.notice(
        recipients,
        format!("审批单已撤回：{}", instance.title),
        format!(
            "{} 撤回了审批单 {}（{}）。",
            instance
                .initiator_name
                .clone()
                .unwrap_or_else(|| "发起人".to_string()),
            instance.instance_no,
            instance.flow_name
        ),
        format!("/flow/instance/{}", instance.id),
        instance.id,
        false,
    );
    tracing::info!(
        instance_id = %instance.id,
        canceled_tasks = pending,
        cancelled_nodes,
        "审批单已撤回"
    );
    Ok(ActionOut {
        instance_id: Some(instance.id),
        instance_status: "WITHDRAWN".to_string(),
        done: true,
        effects,
        ..ActionOut::default()
    })
}

// ==================== 转办 ====================

/// 转办（**事务内**，架构 D12 / §4.8.1）：旧行 `TRANSFERRED` 留痕，新行 `transferred_from` 指向旧行。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn transfer(
    ex: &Transaction,
    task_id: Uuid,
    operator: Uuid,
    to_user_id: Uuid,
    reason: Option<&str>,
) -> DbResult<ActionOut> {
    let Some(task) = model::lock_task(ex, task_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_TASK_NOT_FOUND, "待办不存在"),
        ));
    };
    if task.assignee_id != operator {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(403, error_code::FLOW_TASK_NOT_MINE, "只能转办自己的待办"),
        ));
    }
    if !task.is_pending() {
        return Ok(ActionOut {
            instance_id: Some(task.instance_id),
            node_status: Some(task.status.clone()),
            done: true,
            idempotent: true,
            ..ActionOut::default()
        });
    }
    let Some(node) = model::lock_node(ex, task.instance_node_id).await? else {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                404,
                error_code::FLOW_TASK_NOT_FOUND,
                "待办对应的节点行不存在",
            ),
        ));
    };
    if to_user_id == task.assignee_id {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_TRANSFER_INVALID,
                "不能把待办转办给自己",
            ),
        ));
    }
    let Some(target) = model::user_brief(to_user_id).await? else {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_TRANSFER_INVALID,
                "转办目标用户不存在或已删除",
            ),
        ));
    };
    if !target.active {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_TRANSFER_INVALID,
                format!("转办目标用户 {} 已停用", target.name),
            ),
        ));
    }
    if target.is_robot && !node_allows_robot(&node, ex, task.instance_id).await? {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_TRANSFER_INVALID,
                "转办目标用户是机器人，且该节点未开启 allow_robot",
            ),
        ));
    }

    let new_task_id = Uuid::now_v7();
    // ② 旧行留痕（`TRANSFERRED` + `transferred_to`），历史不丢
    ex.execute_with_params(
        "UPDATE biz_flow_task SET status = 'TRANSFERRED', transferred_to = ?::uuid, \
                comment = COALESCE(?, comment), completed_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::from(&new_task_id),
            Value::Text(reason.unwrap_or("转办").to_string()),
            Value::from(&task.id),
        ],
    )
    .await?;
    // ③ 新行（`transferred_from` 指回旧行）
    let insert = model::insert_task(
        ex,
        &model::NewTask {
            id: new_task_id,
            instance_id: task.instance_id,
            instance_node_id: task.instance_node_id,
            node_key: task.node_key.clone(),
            round: task.round,
            task_kind: task.task_kind.clone(),
            assignee_id: target.id,
            assignee_name: Some(target.name.clone()),
            assignee_source: "USER".to_string(),
            assignee_source_ref: None,
            is_added_sign: false,
            is_self_approval: false,
            is_auto: false,
            status: "PENDING".to_string(),
            comment: None,
            due_at: task.due_at,
            transferred_from: Some(task.id),
        },
    )
    .await;
    let mut idempotent = false;
    if let Err(error) = insert {
        if model::is_pending_duplicate(&error) {
            // 三层去重第 2 层：目标本来就有这条待办 → 幂等成功（不 500）
            idempotent = true;
        } else {
            return Err(error);
        }
    }

    let tally = model::tally_of_node(ex, node.id, node.round).await?;
    model::update_node_counts(ex, node.id, tally).await?;

    let mut effects = Effects::default();
    effects.notice(
        vec![target.id],
        format!("转办给你审批：{}", task.node_key),
        format!(
            "{} 把审批待办转办给你{}。",
            task.assignee_name.clone().unwrap_or_default(),
            reason.map(|text| format!("：{text}")).unwrap_or_default()
        ),
        format!("/flow/instance/{}", new_task_id),
        task.instance_id,
        true,
    );
    Ok(ActionOut {
        instance_id: Some(task.instance_id),
        instance_status: "PENDING".to_string(),
        node_status: Some(node.status),
        done: true,
        idempotent,
        approver_count: tally.total as i32,
        approved_count: tally.approved as i32,
        rejected_count: tally.rejected as i32,
        effects,
        ..ActionOut::default()
    })
}

// ==================== 加签 ====================

/// 并行加签（**事务内**，架构 D11 / §4.8.2）：同节点加人，`应到 + 1`。
///
/// 应到人数在行锁内用 `COUNT(*)` 重算（文档允许 `+=`，本实现更严格：
/// 只有「票数」禁 `+=`，但能现算就现算，省掉一份可能漂移的计数）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn add_sign(
    ex: &Transaction,
    task_id: Uuid,
    operator: Uuid,
    user_ids: &[Uuid],
) -> DbResult<ActionOut> {
    let Some(task) = model::lock_task(ex, task_id).await? else {
        return Ok(ActionOut::deny(
            None,
            ApiError::new(404, error_code::FLOW_TASK_NOT_FOUND, "待办不存在"),
        ));
    };
    if task.assignee_id != operator {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(403, error_code::FLOW_TASK_NOT_MINE, "只能给自己的待办加签"),
        ));
    }
    if !task.is_pending() {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                409,
                error_code::FLOW_TASK_ALREADY_DONE,
                "这条待办已处理，不能再加签",
            ),
        ));
    }
    let Some(node) = model::lock_node(ex, task.instance_node_id).await? else {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                404,
                error_code::FLOW_TASK_NOT_FOUND,
                "待办对应的节点行不存在",
            ),
        ));
    };
    if node.status != "ACTIVE" {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                409,
                error_code::FLOW_ADD_SIGN_INVALID,
                format!("节点当前状态 {}，只有审批中才能加签", node.status),
            ),
        ));
    }
    let mut wanted: Vec<Uuid> = user_ids.to_vec();
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(400, error_code::FLOW_ADD_SIGN_INVALID, "加签人不能为空"),
        ));
    }
    if wanted.len() > ADD_SIGN_MAX_USERS {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_ADD_SIGN_INVALID,
                format!("单次加签最多 {ADD_SIGN_MAX_USERS} 人"),
            ),
        ));
    }

    let allow_robot = node_allows_robot(&node, ex, task.instance_id).await?;
    let mut added: Vec<Uuid> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for user_id in wanted {
        let Some(target) = model::user_brief(user_id).await? else {
            skipped.push(format!("{user_id} 不存在"));
            continue;
        };
        if !target.active {
            skipped.push(format!("{} 已停用", target.name));
            continue;
        }
        if target.is_robot && !allow_robot {
            skipped.push(format!("{} 是机器人且节点未开 allow_robot", target.name));
            continue;
        }
        let insert = model::insert_task(
            ex,
            &model::NewTask {
                id: Uuid::now_v7(),
                instance_id: task.instance_id,
                instance_node_id: task.instance_node_id,
                node_key: task.node_key.clone(),
                round: task.round,
                task_kind: "APPROVAL".to_string(),
                assignee_id: target.id,
                assignee_name: Some(target.name.clone()),
                assignee_source: "USER".to_string(),
                assignee_source_ref: None,
                is_added_sign: true,
                is_self_approval: false,
                is_auto: false,
                status: "PENDING".to_string(),
                comment: None,
                due_at: task.due_at,
                transferred_from: None,
            },
        )
        .await;
        match insert {
            Ok(()) => added.push(target.id),
            Err(error) if model::is_pending_duplicate(&error) => {
                skipped.push(format!("{} 已在该节点审批人里", target.name));
            }
            Err(error) => return Err(error),
        }
    }
    if added.is_empty() {
        return Ok(ActionOut::deny(
            Some(task.instance_id),
            ApiError::new(
                400,
                error_code::FLOW_ADD_SIGN_INVALID,
                format!("没有可以加签的人（{}）", skipped.join("；")),
            ),
        ));
    }

    // 应到人数现算（`approver_count` 是「配置」，用 COUNT(*) 重算最稳）
    let tally = model::tally_of_node(ex, node.id, node.round).await?;
    model::update_node_counts(ex, node.id, tally).await?;

    let mut effects = Effects::default();
    effects.notice(
        added.clone(),
        format!("加签给你审批：{}", node.node_name),
        format!(
            "{} 在节点「{}」加签了你，请及时处理。",
            task.assignee_name.clone().unwrap_or_default(),
            node.node_name
        ),
        format!("/flow/task/{task_id}"),
        task.instance_id,
        true,
    );
    tracing::info!(
        task_id = %task_id,
        added = added.len(),
        skipped = ?skipped,
        "并行加签完成"
    );
    Ok(ActionOut {
        instance_id: Some(task.instance_id),
        instance_status: "PENDING".to_string(),
        node_status: Some(node.status),
        done: true,
        approver_count: tally.total as i32,
        approved_count: tally.approved as i32,
        rejected_count: tally.rejected as i32,
        effects,
        ..ActionOut::default()
    })
}

// ==================== 引擎内部：一次流转 ====================

/// 驱动结果（避免递归 async fn）。
enum Step {
    /// 什么都不用做（幂等命中 / 等人齐 / 走到 END）。
    Idle,
    /// 继续激活这些下游节点。
    Next(Vec<String>),
    /// 业务失败（整单回滚由 handler 决定，引擎只回传）。
    Fail(ApiError),
}

struct Run<'a> {
    ex: &'a Transaction,
    instance: model::InstanceRow,
    snapshot: Snapshot,
    /// 当前轮次（**所有激活都带它**，坑 4 的核心）。
    round: i32,
    /// 本次动作激活的下游节点（出参）。
    next_nodes: Vec<String>,
    /// 锁内收集的副作用。
    effects: Effects,
    /// 实例最终状态（`None` = 仍是 `PENDING`）。
    final_status: Option<String>,
    /// 最近一次写节点后的票数（出参用）。
    approver_count: i32,
    approved_count: i32,
    rejected_count: i32,
}

impl<'a> Run<'a> {
    fn new(ex: &'a Transaction, instance: model::InstanceRow, snapshot: Snapshot) -> Self {
        Self {
            ex,
            instance,
            snapshot,
            round: 1,
            next_nodes: Vec::new(),
            effects: Effects::default(),
            final_status: None,
            approver_count: 0,
            approved_count: 0,
            rejected_count: 0,
        }
    }

    /// 显式队列驱动（替代递归 `async fn`）：按出边优先级逐条推进。
    async fn drive(&mut self, start_keys: Vec<String>) -> DbResult<Option<ApiError>> {
        let mut queue: VecDeque<String> = start_keys.into();
        let mut guard = 0usize;
        while let Some(key) = queue.pop_front() {
            guard += 1;
            // 节点数上限（≤200）+ 每轮最多走完所有节点：防止脏快照造成死循环
            if guard > 1000 {
                return Ok(Some(ApiError::new(
                    500,
                    error_code::FLOW_GRAPH_STRUCTURE,
                    "流程图执行步数超限（可能快照有环），请重新发布流程",
                )));
            }
            match self.step(&key).await? {
                Step::Idle => {}
                Step::Next(targets) => queue.extend(targets),
                Step::Fail(error) => return Ok(Some(error)),
            }
        }
        Ok(None)
    }

    /// 激活一个节点。
    async fn step(&mut self, key: &str) -> DbResult<Step> {
        let Some(spec) = self.spec(key) else {
            return Ok(Step::Fail(ApiError::new(
                500,
                error_code::FLOW_GRAPH_STRUCTURE,
                format!("流程快照里没有节点 {key}"),
            )));
        };
        let round = self.round;

        // 幂等：本轮该节点已经结束 → 什么都不做（防止重复激活下一节点出两套待办，§4.5 实验 2）
        if let Some(existing) = model::find_node(self.ex, self.instance.id, key, round).await?
            && existing.is_terminal()
        {
            return Ok(Step::Idle);
        }

        // 汇聚闸（入度 ≥ 2 才有意义）：**判定必须带 round**（坑 4 的第 5 条）
        let in_edges: Vec<SnapshotEdge> = self
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.to == key)
            .cloned()
            .collect();
        if in_edges.len() >= 2 {
            let mut arrived = 0usize;
            for edge in &in_edges {
                if model::node_arrived(self.ex, self.instance.id, &edge.from, round).await? {
                    arrived += 1;
                }
            }
            let join_and =
                GatewayMode::parse(&spec.join_mode).unwrap_or(GatewayMode::And) == GatewayMode::And;
            if join_and && arrived < in_edges.len() {
                // 等人齐：登记「到达过」但保持 PENDING（不建待办）
                model::touch_node(
                    self.ex,
                    &model::NewNode {
                        id: Uuid::now_v7(),
                        instance_id: self.instance.id,
                        node_id: None,
                        node_key: spec.key.clone(),
                        node_name: spec.name.clone(),
                        node_type: spec.node_type.clone(),
                        round,
                        approval_mode: spec.approval_mode.clone(),
                        approve_ratio: spec
                            .approve_ratio
                            .as_deref()
                            .and_then(|raw| raw.parse::<f64>().ok()),
                        reject_veto: Some(spec.reject_veto),
                    },
                )
                .await?;
                tracing::info!(
                    instance_id = %self.instance.id,
                    node_key = %key,
                    round,
                    arrived,
                    total = in_edges.len(),
                    "汇聚点等人齐（保持 PENDING）"
                );
                return Ok(Step::Idle);
            }
        }

        // 登记本轮到达（幂等：`ON CONFLICT DO UPDATE`）
        model::touch_node(
            self.ex,
            &model::NewNode {
                id: Uuid::now_v7(),
                instance_id: self.instance.id,
                node_id: None,
                node_key: spec.key.clone(),
                node_name: spec.name.clone(),
                node_type: spec.node_type.clone(),
                round,
                approval_mode: spec.approval_mode.clone(),
                approve_ratio: spec
                    .approve_ratio
                    .as_deref()
                    .and_then(|raw| raw.parse::<f64>().ok()),
                reject_veto: Some(spec.reject_veto),
            },
        )
        .await?;
        let Some(row) = model::find_node(self.ex, self.instance.id, key, round).await? else {
            return Ok(Step::Fail(ApiError::new(
                500,
                error_code::FLOW_GRAPH_STRUCTURE,
                format!("节点 {key} 的实例行创建失败"),
            )));
        };
        if row.is_terminal() {
            return Ok(Step::Idle);
        }

        let node_type = NodeType::parse(&spec.node_type).unwrap_or(NodeType::Approval);
        match node_type {
            NodeType::End => {
                model::end_node_row(self.ex, row.id, "APPROVED", Tally::default(), None, false)
                    .await?;
                self.maybe_finish().await?;
                Ok(Step::Idle)
            }
            NodeType::Start => {
                model::end_node_row(self.ex, row.id, "APPROVED", Tally::default(), None, false)
                    .await?;
                Ok(Step::Next(self.out_targets(&spec.key, None, true)))
            }
            NodeType::Condition => {
                let (chosen, failure) = self.pick_condition_edge(&spec).await?;
                if let Some(error) = failure {
                    return Ok(Step::Fail(error));
                }
                let Some(edge) = chosen else {
                    return Ok(Step::Fail(ApiError::new(
                        500,
                        error_code::FLOW_CONDITION_INVALID,
                        format!("条件节点 {} 没有任何可用出边（缺少兜底边）", spec.key),
                    )));
                };
                model::end_node_row(
                    self.ex,
                    row.id,
                    "APPROVED",
                    Tally::default(),
                    Some(&edge.key),
                    false,
                )
                .await?;
                Ok(Step::Next(vec![edge.to]))
            }
            NodeType::Parallel => {
                let targets = self.out_targets(&spec.key, None, false);
                model::end_node_row(self.ex, row.id, "APPROVED", Tally::default(), None, false)
                    .await?;
                Ok(Step::Next(targets))
            }
            NodeType::Cc => {
                let ctx = self.instance.assignee_ctx();
                let Resolution { finalized, trace } =
                    assignee::resolve(self.ex, &spec, &ctx).await?;
                if let Some(error) = finalized.error {
                    return Ok(Step::Fail(error));
                }
                let mut receivers = 0usize;
                for candidate in finalized.approvers.iter().chain(finalized.skipped.iter()) {
                    model::insert_cc(
                        self.ex,
                        &model::NewCc {
                            instance_id: self.instance.id,
                            node_key: spec.key.clone(),
                            round,
                            receiver_id: candidate.user_id,
                            receiver_name: Some(candidate.name.clone()),
                            source_type: candidate.source.as_str().to_string(),
                            source_ref: candidate.source_ref,
                        },
                    )
                    .await?;
                    receivers += 1;
                }
                tracing::info!(
                    instance_id = %self.instance.id,
                    node_key = %spec.key,
                    receivers,
                    trace = ?trace,
                    "抄送节点已扇出（非阻塞）"
                );
                if receivers > 0 {
                    let recipients =
                        model::cc_receivers(self.ex, self.instance.id, &spec.key, round).await?;
                    self.effects.notice(
                        recipients,
                        format!("抄送给你：{}", self.instance.title),
                        format!("审批单 {} 抄送给你知悉。", self.instance.instance_no),
                        format!("/flow/instance/{}", self.instance.id),
                        self.instance.id,
                        false,
                    );
                }
                model::end_node_row(self.ex, row.id, "SKIPPED", Tally::default(), None, false)
                    .await?;
                Ok(Step::Next(self.out_targets(&spec.key, None, false)))
            }
            NodeType::Approval | NodeType::Countersign => {
                let ctx = self.instance.assignee_ctx();
                let Resolution { finalized, trace } =
                    assignee::resolve(self.ex, &spec, &ctx).await?;
                if let Some(error) = finalized.error {
                    return Ok(Step::Fail(error));
                }
                tracing::info!(
                    instance_id = %self.instance.id,
                    node_key = %spec.key,
                    round,
                    approvers = finalized.approvers.len(),
                    skipped = finalized.skipped.len(),
                    auto_pass = finalized.auto_pass,
                    trace = ?trace,
                    "审批人解析完成（冻结成行）"
                );

                // 自审 SKIP 的留痕行（**不许静默丢弃**，坑 3）
                for candidate in &finalized.skipped {
                    let insert = model::insert_task(
                        self.ex,
                        &model::NewTask {
                            id: Uuid::now_v7(),
                            instance_id: self.instance.id,
                            instance_node_id: row.id,
                            node_key: spec.key.clone(),
                            round,
                            task_kind: "APPROVAL".to_string(),
                            assignee_id: candidate.user_id,
                            assignee_name: Some(candidate.name.clone()),
                            assignee_source: candidate.source.as_str().to_string(),
                            assignee_source_ref: candidate.source_ref,
                            is_added_sign: false,
                            is_self_approval: true,
                            is_auto: false,
                            status: "SKIPPED".to_string(),
                            comment: Some(
                                "审批人是发起人本人（self_approval=SKIP），已跳过并留痕"
                                    .to_string(),
                            ),
                            due_at: None,
                            transferred_from: None,
                        },
                    )
                    .await;
                    if let Err(error) = insert
                        && !model::is_pending_duplicate(&error)
                    {
                        return Err(error);
                    }
                }

                if finalized.approvers.is_empty() {
                    // 空审批人 SKIP：节点跳过 + `auto_passed_nodes + 1`（派生值，见 sync_auto_passed）
                    model::end_node_row(self.ex, row.id, "SKIPPED", Tally::default(), None, true)
                        .await?;
                    model::sync_auto_passed(self.ex, self.instance.id).await?;
                    self.effects.notice(
                        vec![self.instance.initiator_id],
                        format!("节点自动跳过：{}", spec.name),
                        format!(
                            "审批单 {} 的节点「{}」没有有效审批人（自审跳过 / 角色内 0 人），已自动跳过。",
                            self.instance.instance_no, spec.name
                        ),
                        format!("/flow/instance/{}", self.instance.id),
                        self.instance.id,
                        true,
                    );
                    return Ok(Step::Next(self.out_targets(&spec.key, None, false)));
                }

                // 落 PENDING 待办（三层去重第 2 层：23505 当幂等成功，不 500）
                let mut created: Vec<Uuid> = Vec::new();
                for candidate in &finalized.approvers {
                    let insert = model::insert_task(
                        self.ex,
                        &model::NewTask {
                            id: Uuid::now_v7(),
                            instance_id: self.instance.id,
                            instance_node_id: row.id,
                            node_key: spec.key.clone(),
                            round,
                            task_kind: "APPROVAL".to_string(),
                            assignee_id: candidate.user_id,
                            assignee_name: Some(candidate.name.clone()),
                            assignee_source: candidate.source.as_str().to_string(),
                            assignee_source_ref: candidate.source_ref,
                            is_added_sign: false,
                            is_self_approval: candidate.user_id == self.instance.initiator_id,
                            is_auto: false,
                            status: "PENDING".to_string(),
                            comment: None,
                            due_at: None,
                            transferred_from: None,
                        },
                    )
                    .await;
                    match insert {
                        Ok(()) => created.push(candidate.user_id),
                        // 三层去重第 2 层：并发下另一条路径已经落了同一行 → 幂等成功
                        Err(error) if model::is_pending_duplicate(&error) => {
                            tracing::info!(
                                node_key = %spec.key,
                                user = %candidate.user_id,
                                "待办已存在（23505 当幂等成功）"
                            );
                        }
                        Err(error) => return Err(error),
                    }
                }

                let tally = model::tally_of_node(self.ex, row.id, round).await?;
                model::activate_node_row(self.ex, row.id, tally.total as i32).await?;
                self.approver_count = tally.total as i32;
                self.approved_count = tally.approved as i32;
                self.rejected_count = tally.rejected as i32;

                // 通知本轮新增的审批人（锁外发送，这里只收集）
                if !created.is_empty() {
                    self.effects.notice(
                        created,
                        format!("待你审批：{}", self.instance.title),
                        format!(
                            "审批单 {}（{}）到达节点「{}」，请及时处理。",
                            self.instance.instance_no, self.instance.flow_name, spec.name
                        ),
                        format!("/flow/instance/{}", self.instance.id),
                        self.instance.id,
                        true,
                    );
                }
                Ok(Step::Idle)
            }
        }
    }

    /// 取节点配置（快照）。
    fn spec(&self, key: &str) -> Option<SnapshotNode> {
        self.snapshot
            .nodes
            .iter()
            .find(|node| node.key == key)
            .cloned()
    }

    /// 出边目标（按 `priority` 升序，保证可复现）。
    fn out_targets(&self, key: &str, only_edge: Option<&str>, _is_start: bool) -> Vec<String> {
        let mut edges: Vec<&SnapshotEdge> = self
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.from == key)
            .filter(|edge| only_edge.map(|wanted| edge.key == wanted).unwrap_or(true))
            .collect();
        edges.sort_by_key(|edge| (edge.priority, edge.key.clone()));
        edges.into_iter().map(|edge| edge.to.clone()).collect()
    }

    /// 条件 / OR 分叉：按优先级求值，取第一条为真的边，都不中走 `is_default`。
    async fn pick_condition_edge(
        &self,
        spec: &SnapshotNode,
    ) -> DbResult<(Option<SnapshotEdge>, Option<ApiError>)> {
        let mut edges: Vec<&SnapshotEdge> = self
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.from == spec.key)
            .collect();
        edges.sort_by_key(|edge| (edge.priority, edge.key.clone()));
        let context = self.eval_context().await?;
        for edge in &edges {
            if edge.is_default {
                continue;
            }
            let Some(raw) = edge.condition.as_ref() else {
                continue;
            };
            let condition = match graph::parse_condition(raw) {
                Ok(condition) => condition,
                Err(message) => {
                    return Ok((
                        None,
                        Some(ApiError::new(
                            500,
                            error_code::FLOW_CONDITION_INVALID,
                            format!("条件解析失败（边 {}）：{message}", edge.key),
                        )),
                    ));
                }
            };
            match graph::eval(&condition, &context) {
                Ok(true) => return Ok((Some((*edge).clone()), None)),
                Ok(false) => {}
                Err(error) => {
                    // 字段缺失 → 视为 false（不是错误）：条件里引用了发起人没填的变量时，
                    // 业务语义是「这条分支不成立」，而不是整单报错。
                    tracing::warn!(edge = %edge.key, error = ?error, "条件求值失败，视为不成立");
                }
            }
        }
        Ok((
            edges
                .iter()
                .find(|edge| edge.is_default)
                .map(|edge| (*edge).clone()),
            None,
        ))
    }

    /// 条件求值上下文：`form` = `instance.form_data`，`initiator` / `instance` 来自实例行。
    ///
    /// **必须覆盖 `graph::field_allowed` 白名单里的每一个字段**，否则条件里引用了
    /// 缺字段的分支会永远 `EvalError::Missing` → 静默走兜底边（只有 WARN 没有
    /// ERROR），配置了也不生效。
    ///
    /// 白名单（`graph.rs:1422`）：
    ///   - `form.*`
    ///   - `initiator.id` / `initiator.name` / `initiator.dept_id`
    ///   - `instance.biz_type` / `instance.biz_no` / `instance.title`
    ///   - `task.status` / `task.priority` / `task.estimate_hours` / `task.title`
    ///
    /// `task.*` 仅 `TASK_ACCEPT` 场景填（`biz_type = "TASK_ACCEPT"` 且 `biz_id`
    /// 指向 `biz_task.id`）；其他流程留空 Map —— 条件里用到就是 Missing → 兜底边。
    async fn eval_context(&self) -> DbResult<EvalContext> {
        let mut form = Map::new();
        if let Json::Object(map) = &self.instance.form_data {
            form = map.clone();
        }

        // ── initiator：白名单三项全覆盖（id / name / dept_id）──
        let mut initiator = Map::new();
        initiator.insert(
            "id".to_string(),
            Json::String(self.instance.initiator_id.to_string()),
        );
        if let Some(name) = &self.instance.initiator_name {
            initiator.insert("name".to_string(), Json::String(name.clone()));
        }
        if let Some(dept) = self.instance.initiator_dept_id {
            initiator.insert("dept_id".to_string(), Json::String(dept.to_string()));
        }

        // ── instance：白名单三项全覆盖（biz_type / biz_no / title）
        //    id / biz_id 不在白名单，但保留供将来放宽（field_allowed 会先拒）──
        let mut instance = Map::new();
        instance.insert("id".to_string(), Json::String(self.instance.id.to_string()));
        instance.insert(
            "biz_type".to_string(),
            Json::String(self.instance.biz_type.clone()),
        );
        if let Some(biz_id) = self.instance.biz_id {
            instance.insert("biz_id".to_string(), Json::String(biz_id.to_string()));
        }
        if let Some(biz_no) = &self.instance.biz_no {
            instance.insert("biz_no".to_string(), Json::String(biz_no.clone()));
        }
        if !self.instance.title.is_empty() {
            instance.insert(
                "title".to_string(),
                Json::String(self.instance.title.clone()),
            );
        }

        // ── task：仅 TASK_ACCEPT 场景查 biz_task（白名单四项：status/priority/estimate_hours/title）──
        let mut task = Map::new();
        if self.instance.biz_type == "TASK_ACCEPT"
            && let Some(task_id) = self.instance.biz_id
        {
            // estimate_hours 是 numeric(6,1)，::float8 转数字（JSON 里是 number 而非 string）
            let rows = self
                .ex
                .find_with_params(
                    "SELECT title, status, priority, \
                                estimate_hours::float8 AS estimate_hours \
                           FROM biz_task \
                          WHERE id = ?::uuid AND deleted_at IS NULL",
                    vec![Value::from(&task_id)],
                )
                .await?;
            if let Some(row) = rows.first() {
                if let Ok(title) = row.get_typed::<String>("title") {
                    task.insert("title".to_string(), Json::String(title));
                }
                if let Ok(status) = row.get_typed::<String>("status") {
                    task.insert("status".to_string(), Json::String(status));
                }
                if let Ok(priority) = row.get_typed::<String>("priority") {
                    task.insert("priority".to_string(), Json::String(priority));
                }
                if let Ok(Some(hours)) = row.get_typed::<Option<f64>>("estimate_hours")
                    && let Some(num) = serde_json::Number::from_f64(hours)
                {
                    task.insert("estimate_hours".to_string(), Json::Number(num));
                }
            }
        }

        Ok(EvalContext {
            form,
            initiator,
            instance,
            task,
        })
    }

    /// 实例是否该结束了（没有 ACTIVE / 等待中的节点）。
    async fn maybe_finish(&mut self) -> DbResult<()> {
        let inflight = model::count_inflight(self.ex, self.instance.id).await?;
        if inflight == 0 {
            self.final_status = Some("APPROVED".to_string());
            let auto_passed = model::sync_auto_passed(self.ex, self.instance.id).await?;
            self.effects.notice(
                vec![self.instance.initiator_id],
                format!("审批已通过：{}", self.instance.title),
                format!(
                    "审批单 {}（{}）已全部通过{}。",
                    self.instance.instance_no,
                    self.instance.flow_name,
                    if auto_passed > 0 {
                        format!("，其中 {auto_passed} 个节点因无有效审批人自动通过")
                    } else {
                        String::new()
                    }
                ),
                format!("/flow/instance/{}", self.instance.id),
                self.instance.id,
                false,
            );
        }
        Ok(())
    }

    /// 退回（架构 §4.6）：当前节点已 `REJECTED`，这里做「6 件状态回滚」的其余部分
    /// 并把流程拉到目标节点的新轮次。
    async fn apply_return(
        &mut self,
        node: &model::NodeRow,
        return_to: &ReturnTo,
        comment: &str,
    ) -> DbResult<Option<ApiError>> {
        // ③ 掐断其它 ACTIVE / 等待中的节点（并行分支上的幽灵待办必须清掉）
        let cancelled =
            model::cancel_active_nodes(self.ex, self.instance.id, Some(node.id)).await?;
        if cancelled > 0 {
            model::cancel_pending_of_instance(self.ex, self.instance.id, "流程被退回，该节点作废")
                .await?;
        }

        if *return_to == ReturnTo::Final {
            self.final_status = Some("REJECTED".to_string());
            self.effects.notice(
                vec![self.instance.initiator_id],
                format!("审批已驳回：{}", self.instance.title),
                format!(
                    "审批单 {} 被驳回并终止：{}",
                    self.instance.instance_no, comment
                ),
                format!("/flow/instance/{}", self.instance.id),
                self.instance.id,
                true,
            );
            return Ok(None);
        }

        // ④ 目标节点 + 新轮次（`max(round) + 1`，历史行一行不动）
        //
        // 「上一节点」在**汇聚节点**上是多值的：PARALLEL(AND) 分叉后两支都已通过，
        // 汇聚点驳回时两支都得回落（否则另一支停在上一轮，AND 汇聚闸永远等不齐 → 卡死）。
        // 因此这里算的是 `targets: Vec<String>`，全部按新轮次重新激活。
        let starts = start_key_of(&self.snapshot);
        let targets: Vec<String> = match return_to {
            ReturnTo::Node(key) => {
                if self.spec(key).is_none() {
                    return Ok(Some(ApiError::new(
                        400,
                        error_code::FLOW_RETURN_TARGET_INVALID,
                        format!("退回目标节点 {key} 不在本流程图上"),
                    )));
                }
                vec![key.clone()]
            }
            ReturnTo::Initiator => starts
                .clone()
                .map(|key| vec![key])
                .ok_or_else(|| DbError::InvalidValue("流程快照里没有 START 节点".to_string()))?,
            ReturnTo::Prev | ReturnTo::Final => {
                let upstream = self.passed_upstream_sources(&node.node_key).await?;
                if upstream.is_empty() {
                    starts.clone().map(|key| vec![key]).ok_or_else(|| {
                        DbError::InvalidValue("流程快照里没有 START 节点".to_string())
                    })?
                } else {
                    upstream
                }
            }
        };

        let next_round = model::max_round(self.ex, self.instance.id).await? + 1;
        if next_round > MAX_ROUND {
            return Ok(Some(ApiError::new(
                409,
                error_code::FLOW_RETURN_TARGET_INVALID,
                format!("退回轮次已达上限 {MAX_ROUND}，请人工介入"),
            )));
        }
        self.round = next_round;

        let target_key = targets[0].clone();
        let target_is_start = Some(&target_key) == starts.as_ref();
        let go_initiator = *return_to == ReturnTo::Initiator
            || (target_is_start && matches!(return_to, ReturnTo::Prev));

        if go_initiator {
            // 退发起人：START 节点 `round + 1` + 一条 `RESUBMIT` 待办（D8 / §4.11 时序 3）
            let spec = self
                .spec(&target_key)
                .ok_or_else(|| DbError::InvalidValue(format!("流程快照里没有节点 {target_key}")))?;
            model::touch_node(
                self.ex,
                &model::NewNode {
                    id: Uuid::now_v7(),
                    instance_id: self.instance.id,
                    node_id: None,
                    node_key: spec.key.clone(),
                    node_name: spec.name.clone(),
                    node_type: spec.node_type.clone(),
                    round: next_round,
                    approval_mode: None,
                    approve_ratio: None,
                    reject_veto: None,
                },
            )
            .await?;
            let Some(start_row) =
                model::find_node(self.ex, self.instance.id, &target_key, next_round).await?
            else {
                return Ok(Some(ApiError::new(
                    500,
                    error_code::FLOW_GRAPH_STRUCTURE,
                    "退回到发起人时未能创建 START 节点行",
                )));
            };
            model::activate_node_row(self.ex, start_row.id, 1).await?;
            let name = self.instance.initiator_name.clone();
            let insert = model::insert_task(
                self.ex,
                &model::NewTask {
                    id: Uuid::now_v7(),
                    instance_id: self.instance.id,
                    instance_node_id: start_row.id,
                    node_key: target_key.clone(),
                    round: next_round,
                    task_kind: "RESUBMIT".to_string(),
                    assignee_id: self.instance.initiator_id,
                    assignee_name: name.clone(),
                    assignee_source: "INITIATOR".to_string(),
                    assignee_source_ref: None,
                    is_added_sign: false,
                    is_self_approval: false,
                    is_auto: false,
                    status: "PENDING".to_string(),
                    comment: Some(format!("被驳回，请修改后重新提交：{comment}")),
                    due_at: None,
                    transferred_from: None,
                },
            )
            .await;
            if let Err(error) = insert
                && !model::is_pending_duplicate(&error)
            {
                return Err(error);
            }
            self.next_nodes.push(target_key);
            self.effects.notice(
                vec![self.instance.initiator_id],
                format!("审批被退回：{}", self.instance.title),
                format!(
                    "审批单 {} 被 {} 退回给你：{}",
                    self.instance.instance_no, node.node_name, comment
                ),
                format!("/flow/instance/{}", self.instance.id),
                self.instance.id,
                true,
            );
            return Ok(None);
        }

        // 退上一节点 / 指定节点：所有目标节点开新轮次行并重新解析审批人
        self.next_nodes.extend(targets.iter().cloned());
        let failure = self.drive(targets).await?;
        Ok(failure)
    }

    /// 本轮「已通过的上游来源节点」：
    ///
    /// - 入度 = 1（链式）：BFS 反向找**最近**的那个已通过节点；
    /// - 入度 ≥ 2（汇聚）：**所有**入边来源里本轮已通过的节点 —— 汇聚点驳回必须整组回落，
    ///   只回落一支会让 AND 汇聚闸永远等不齐（坑 4 的变体）。
    async fn passed_upstream_sources(&self, from_key: &str) -> DbResult<Vec<String>> {
        let sources: Vec<String> = self
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.to == from_key)
            .map(|edge| edge.from.clone())
            .collect();
        if sources.len() >= 2 {
            let mut arrived = Vec::new();
            for key in sources {
                if model::node_arrived(self.ex, self.instance.id, &key, self.round).await? {
                    arrived.push(key);
                }
            }
            arrived.sort();
            arrived.dedup();
            return Ok(arrived);
        }
        Ok(self
            .nearest_passed_upstream(from_key)
            .await?
            .into_iter()
            .collect())
    }

    /// 本轮「最近的一个已通过上游节点」（BFS 反向，逐层找）。入度 = 1 的链式流程用。
    async fn nearest_passed_upstream(&self, from_key: &str) -> DbResult<Option<String>> {
        let mut frontier: VecDeque<String> = VecDeque::new();
        let mut visited: Vec<String> = vec![from_key.to_string()];
        // 直接上游先入队
        for edge in self
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.to == from_key)
        {
            frontier.push_back(edge.from.clone());
        }
        while let Some(key) = frontier.pop_front() {
            if visited.contains(&key) {
                continue;
            }
            visited.push(key.clone());
            if model::node_arrived(self.ex, self.instance.id, &key, self.round).await? {
                return Ok(Some(key));
            }
            for edge in self.snapshot.edges.iter().filter(|edge| edge.to == key) {
                frontier.push_back(edge.from.clone());
            }
        }
        Ok(None)
    }
}

// ==================== 引擎内部小工具 ====================

// SCOPE-EXEMPT: 引擎只按「实例已绑定的 `instance.version_id`」读快照、按 id 读实例 / 节点 / 待办，
// SCOPE-EXEMPT: 全部是**单条按主键查询**（`WHERE id = ?`），没有列表 / 分页；可见性与数据范围
// SCOPE-EXEMPT: 的判定在 handler（`instance_detail` / `page_instances`）与
// SCOPE-EXEMPT: `model::page_instances`（那里接了 `scope::filter_for`）。引擎在事务里再注入一次
// SCOPE-EXEMPT: 数据范围只会把「已被授权的流转」打断（例如并行分支上的节点由外部门审批）。

/// 读发布快照（**引擎唯一的图来源**：只读 `version.graph_snapshot`，不 JOIN 定义行，D2）。
async fn load_snapshot(ex: &Transaction, version_id: Uuid) -> DbResult<Snapshot> {
    let rows = ex
        .find_with_params(
            "SELECT graph_snapshot::text AS snapshot_text \
               FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(&version_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(format!(
            "流程版本 {version_id} 不存在"
        )));
    };
    let text: Option<String> = row.get_typed("snapshot_text")?;
    let Some(text) = text.filter(|value| !value.trim().is_empty()) else {
        return Err(DbError::InvalidValue(format!(
            "流程版本 {version_id} 尚未发布（没有 graph_snapshot）"
        )));
    };
    graph::parse_snapshot(&text).map_err(DbError::InvalidValue)
}

fn start_keys_of(snapshot: &Snapshot) -> Vec<String> {
    let mut keys: Vec<String> = snapshot
        .nodes
        .iter()
        .filter(|node| node.node_type.eq_ignore_ascii_case("START"))
        .map(|node| node.key.clone())
        .collect();
    keys.sort();
    keys
}

fn start_key_of(snapshot: &Snapshot) -> Option<String> {
    start_keys_of(snapshot).into_iter().next()
}

/// 节点是否允许机器人审批（`allow_robot` 在快照上，`node` 是实例行所以要回快照取）。
async fn node_allows_robot(
    node: &model::NodeRow,
    ex: &Transaction,
    instance_id: Uuid,
) -> DbResult<bool> {
    let rows = ex
        .find_with_params(
            "SELECT i.version_id AS version_id FROM biz_flow_instance i WHERE i.id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    let version_id: Uuid = row.get_typed("version_id")?;
    let snapshot = load_snapshot(ex, version_id).await?;
    Ok(snapshot
        .nodes
        .iter()
        .find(|spec| spec.key == node.node_key)
        .map(|spec| spec.allow_robot)
        .unwrap_or(false))
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ==================================================================
    // F1 回归：eval_context 必须填满 graph::FIXED_FIELDS 白名单
    // ==================================================================
    //
    // 背景：修复前 eval_context() 只填了 6/11 个白名单字段（缺
    // initiator.name / instance.title / instance.biz_no / task.*），
    // 导致存量条件边里用了这些字段的分支永远 EvalError::Missing
    // → 静默走兜底边（只有 WARN 无 ERROR），配置了也不生效。
    //
    // 两层测试：
    //   ① 结构层：EvalContext 能 resolve 白名单每个字段
    //   ② 源码层：eval_context() 函数体真的 insert 了每个字段（防退化）

    /// ① 结构层：EvalContext 能承载白名单所有字段。
    #[test]
    fn eval_context_struct_resolves_every_whitelisted_field() {
        use serde_json::json;

        let mut ctx = EvalContext::default();
        ctx.form.insert("amount".into(), json!(100));
        ctx.initiator.insert("id".into(), json!("u1"));
        ctx.initiator.insert("name".into(), json!("张三"));
        ctx.initiator.insert("dept_id".into(), json!("d1"));
        ctx.instance.insert("biz_type".into(), json!("TASK_ACCEPT"));
        ctx.instance.insert("biz_no".into(), json!("TSK-001"));
        ctx.instance.insert("title".into(), json!("验收任务"));
        ctx.task.insert("status".into(), json!("TODO"));
        ctx.task.insert("priority".into(), json!("P0"));
        ctx.task.insert("estimate_hours".into(), json!(8.0));
        ctx.task.insert("title".into(), json!("改个按钮"));

        // graph::FIXED_FIELDS 里的每一项都必须能 resolve
        for field in graph::FIXED_FIELDS {
            assert!(
                ctx.resolve(field).is_ok(),
                "白名单字段 {field} 必须能从 EvalContext 解析出来（否则条件边静默失效）"
            );
        }

        // form.* 走前缀规则，单独验一条
        assert!(ctx.resolve("form.amount").is_ok());
    }

    /// ② 源码层：eval_context() 函数体必须 insert 白名单每个字段。
    ///
    /// 这条测试看着丑，但它直接锁住了 F1 的根因：`task: Map::new()` 这种
    /// 「结构存在但从不填充」的写法，结构层测试抓不到，只有源码断言能抓。
    #[test]
    fn eval_context_fills_every_root_in_source() {
        let src = include_str!("engine.rs");

        // 抽出 eval_context 函数体（从签名到下一个成员函数注释之前）
        let start = src
            .find("async fn eval_context(&self) -> DbResult<EvalContext>")
            .expect("eval_context 必须返回 DbResult<EvalContext>（带 ? 的错误传播）");
        let body_end = src[start..]
            .find(
                "
    /// 实例是否该结束了",
            )
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..body_end];

        // 白名单里除 form.* 之外的每个 leaf 都必须有对应的 insert
        let required = [
            ("initiator", "id"),
            ("initiator", "name"),
            ("initiator", "dept_id"),
            ("instance", "biz_type"),
            ("instance", "biz_no"),
            ("instance", "title"),
            ("task", "title"),
            ("task", "status"),
            ("task", "priority"),
            ("task", "estimate_hours"),
        ];
        for (root, leaf) in required {
            let root_pos = body.find(&format!("{root}.insert(")).unwrap_or_else(|| {
                panic!("eval_context() 里找不到 {root}.insert( —— 整类字段缺失")
            });
            let window = &body[root_pos..std::cmp::min(body.len(), root_pos + 800)];
            let leaf_pat = format!("\"{leaf}\"");
            if !window.contains(&leaf_pat) {
                eprintln!("[F1 失败] 缺 {root}.{leaf}");
                eprintln!("  查找窗口: {window:?}");
                panic!("eval_context() 必须填 {root}.{leaf}");
            }
        }

        // task 必须真的查库（不是空 Map）
        assert!(
            body.contains("TASK_ACCEPT") && body.contains("biz_task"),
            "task.* 必须在 TASK_ACCEPT 场景下从 biz_task 查（不能留空 Map）"
        );
    }

    fn tally(a: i64, r: i64, p: i64) -> Tally {
        Tally {
            approved: a,
            rejected: r,
            pending: p,
            total: a + r + p,
        }
    }

    #[test]
    fn countersign_all_waits_for_everyone() {
        // 验收 7：ALL 3 人，2 同意 1 未审 → 仍等待；第 3 人同意 → 通过
        assert_eq!(
            judge(ApprovalMode::All, None, false, tally(2, 0, 1)),
            Verdict::Wait
        );
        assert_eq!(
            judge(ApprovalMode::All, None, false, tally(3, 0, 0)),
            Verdict::Pass
        );
        // 全员表过态且有驳回 → 驳回
        assert_eq!(
            judge(ApprovalMode::All, None, false, tally(2, 1, 0)),
            Verdict::Reject
        );
    }

    #[test]
    fn countersign_ratio_passes_early() {
        // 验收 8：RATIO=0.6、5 人，3 同意 → 提前通过（不等剩余 2 人）
        assert_eq!(
            judge(ApprovalMode::Ratio, Some(0.6), false, tally(3, 0, 2)),
            Verdict::Pass
        );
        // 2 同意 3 未审 → 还没到阈值，等待
        assert_eq!(
            judge(ApprovalMode::Ratio, Some(0.6), false, tally(2, 0, 3)),
            Verdict::Wait
        );
        // 2 同意 2 驳回 1 未审、阈值 0.8 → 剩下的全同意也只有 3/5 = 0.6 < 0.8 → 提前判死
        assert_eq!(
            judge(ApprovalMode::Ratio, Some(0.8), false, tally(2, 2, 1)),
            Verdict::Reject
        );
    }

    #[test]
    fn countersign_any_passes_on_first_vote() {
        // 验收 9：ANY 3 人，1 人同意 → 立即通过
        assert_eq!(
            judge(ApprovalMode::Any, None, false, tally(1, 0, 2)),
            Verdict::Pass
        );
        // 都投完但没人同意 → 驳回
        assert_eq!(
            judge(ApprovalMode::Any, None, false, tally(0, 3, 0)),
            Verdict::Reject
        );
    }

    #[test]
    fn veto_is_orthogonal_to_mode() {
        // 验收 10：RATIO + reject_veto，3 同意但 1 人否决 → 立即驳回（即使比例已达标）
        assert_eq!(
            judge(ApprovalMode::Ratio, Some(0.6), true, tally(3, 1, 1)),
            Verdict::Reject
        );
        // 同样的票数、关掉 veto → 比例已达标 → 通过
        assert_eq!(
            judge(ApprovalMode::Ratio, Some(0.6), false, tally(3, 1, 1)),
            Verdict::Pass
        );
        // ANY + veto：一人同意一人反对 → 驳回优先
        assert_eq!(
            judge(ApprovalMode::Any, None, true, tally(1, 1, 1)),
            Verdict::Reject
        );
    }

    #[test]
    fn zero_assignees_auto_pass() {
        assert_eq!(
            judge(ApprovalMode::All, None, true, tally(0, 0, 0)),
            Verdict::AutoPass
        );
    }

    #[test]
    fn single_sign_matrix() {
        // 单签 = ALL + n = 1
        assert_eq!(
            judge(ApprovalMode::All, None, true, tally(1, 0, 0)),
            Verdict::Pass
        );
        assert_eq!(
            judge(ApprovalMode::All, None, false, tally(0, 1, 0)),
            Verdict::Reject
        );
    }

    #[test]
    fn verdict_matrix_is_exhaustive() {
        // 「会签 3 语义 × veto」全矩阵（验收 25）：3 人、票型枚举 10 种 × 3 模式 × 2 veto = 60 组，
        // 断言每一组都落在 4 个判定里且有明确语义（不是靠 assert 不 panic，而是钉住关键组合）。
        let modes = [ApprovalMode::All, ApprovalMode::Ratio, ApprovalMode::Any];
        let mut checked = 0;
        for mode in modes {
            for veto in [true, false] {
                for a in 0..=3i64 {
                    for r in 0..=(3 - a) {
                        let p = 3 - a - r;
                        let verdict = judge(mode, Some(0.6), veto, tally(a, r, p));
                        if veto && r >= 1 {
                            assert_eq!(
                                verdict,
                                Verdict::Reject,
                                "veto 必须短路：{mode:?} a={a} r={r}"
                            );
                        }
                        if a == 3 {
                            assert_eq!(verdict, Verdict::Pass, "{mode:?} 全员同意必须通过");
                        }
                        if a == 0 && p == 0 && r > 0 {
                            assert_eq!(verdict, Verdict::Reject, "{mode:?} 全反对必须驳回");
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 60, "3 模式 × 2 veto × 10 票型 = 60 组");
    }

    #[test]
    fn instance_state_machine_5x5_matrix() {
        // 状态机 5×5 矩阵（验收 25）：只有 PENDING 的 4 条出边 + 原地不动合法
        let states = ["PENDING", "APPROVED", "REJECTED", "CANCELLED", "WITHDRAWN"];
        let legal = [
            ("PENDING", "APPROVED"),
            ("PENDING", "REJECTED"),
            ("PENDING", "CANCELLED"),
            ("PENDING", "WITHDRAWN"),
        ];
        let mut allowed = 0;
        for from in states {
            for to in states {
                let expected = from == to || legal.contains(&(from, to));
                assert_eq!(
                    transition_allowed(from, to),
                    expected,
                    "{from} → {to} 的判定不对"
                );
                if transition_allowed(from, to) && from != to {
                    allowed += 1;
                }
            }
        }
        assert_eq!(
            allowed, 4,
            "PENDING 之外必须没有任何出边（终态不可复活，§4.1 #7/#8）"
        );

        // 引擎动作 → 迁移
        assert_eq!(
            instance_transition(InstanceAction::Finish, "PENDING"),
            Some("APPROVED")
        );
        assert_eq!(
            instance_transition(InstanceAction::Finish, "APPROVED"),
            Some("APPROVED"),
            "同状态再写一次是幂等（bump_instance 的 CAS 负责只让一个人推进）"
        );
        assert_eq!(
            instance_transition(InstanceAction::Withdraw, "APPROVED"),
            None,
            "终态之间不能互转（REJECTED/WITHDRAWN/CANCELLED 永不复活）"
        );
        assert_eq!(
            instance_transition(InstanceAction::Start, "REJECTED"),
            None,
            "终态不能回 PENDING（要重来就新发起，架构 §4.1 #8）"
        );
        assert_eq!(
            instance_transition(InstanceAction::Withdraw, "PENDING"),
            Some("WITHDRAWN")
        );
    }

    #[test]
    fn no_accumulator_on_vote_counts() {
        // 红线自检（验收 12 的静态侧）：本文件不许出现「票数自增」的写法。
        // 判据字符串用 concat! 拼出来 —— 直接写字面量会让 include_str! 把断言自己算进去。
        let source = include_str!("engine.rs");
        for needle in [
            concat!("approved_count", " += "),
            concat!("rejected_count", " += "),
            concat!("approved_count", " + 1"),
            concat!("rejected_count", " + 1"),
        ] {
            assert!(
                !source.contains(needle),
                "票数禁止自增（D15：必须 COUNT(*) 现算）：{needle}"
            );
        }
    }

    #[test]
    fn return_to_parsing() {
        assert_eq!(ReturnTo::parse(""), ReturnTo::Prev);
        assert_eq!(ReturnTo::parse("prev"), ReturnTo::Prev);
        assert_eq!(ReturnTo::parse("INITIATOR"), ReturnTo::Initiator);
        assert_eq!(ReturnTo::parse("none"), ReturnTo::Final);
        assert_eq!(
            ReturnTo::parse("n_approve_1"),
            ReturnTo::Node("n_approve_1".to_string())
        );
        assert!(ReturnTo::Node("x".to_string()).needs_admin());
        assert!(ReturnTo::Final.needs_admin());
        assert!(!ReturnTo::Prev.needs_admin());
        assert!(!ReturnTo::Initiator.needs_admin());
    }

    #[test]
    fn start_keys_are_detected() {
        let snapshot = Snapshot {
            schema_version: graph::SNAPSHOT_SCHEMA_VERSION,
            nodes: vec![
                graph::SnapshotNode {
                    key: "n_a".to_string(),
                    name: "开始".to_string(),
                    node_type: "START".to_string(),
                    assignee_type: None,
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
                    config: Json::Object(Map::new()),
                    assignees: Vec::new(),
                },
                graph::SnapshotNode {
                    key: "n_b".to_string(),
                    name: "结束".to_string(),
                    node_type: "END".to_string(),
                    assignee_type: None,
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
                    config: Json::Object(Map::new()),
                    assignees: Vec::new(),
                },
            ],
            edges: Vec::new(),
        };
        assert_eq!(start_keys_of(&snapshot), vec!["n_a".to_string()]);
        assert_eq!(start_key_of(&snapshot), Some("n_a".to_string()));
    }

    #[test]
    fn effects_dedupe_recipients() {
        let mut effects = Effects::default();
        let instance = Uuid::now_v7();
        effects.notice(
            vec![Uuid::from_u128(2), Uuid::from_u128(1), Uuid::from_u128(2)],
            "标题",
            "内容",
            "/flow/instance/1",
            instance,
            true,
        );
        assert_eq!(effects.notices.len(), 1);
        assert_eq!(effects.notices[0].recipients.len(), 2, "收件人必须去重");
        // 空收件人 → 不发
        effects.notice(Vec::new(), "标题", "内容", "/x", instance, false);
        assert_eq!(effects.notices.len(), 1);
    }
}
