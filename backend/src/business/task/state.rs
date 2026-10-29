//! 任务状态机与进度算法（批次 C+ · T2）—— **纯逻辑**，不碰数据库。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §二（状态机）/ §三（进度模型）、
//! `docs/云枢设计修正补丁-20261029.md` 修正 8（通知分级）。
//!
//! # 为什么把状态机单独抽成一个文件
//!
//! 状态机有三件容易写错、且**写错了不会报错**的事：
//!
//! 1. **白名单**：`from → to` 的组合一共 5×5 = 25 种，卡面只放行 10 种。
//!    写成 `if` 链很容易漏判（典型事故：`DONE → CANCELLED` 能过、`TODO → DONE` 跳级能过）。
//!    这里用**一张常量表** [`TRANSITIONS`] 表达全部合法流转，表外的组合一律
//!    [`TransitionError::IllegalPair`] —— 判据是「查表」，不是「记忆」。
//! 2. **谁能推**：同一对 `from → to` 对不同角色有不同权限（`REVIEW → DONE` 只有验收人能做，
//!    负责人自己点「完成」必须被拒）。角色用位掩码表达，查表时一次 `&` 判定。
//! 3. **副作用**：进入 DOING 要抬进度下限、进入 REVIEW/DONE 要拉满、重开要把进度归位。
//!    这些副作用跟着流转走（[`ProgressEffect`] / [`DoneAtEffect`]），**不散落在 handler 的
//!    `match` 分支里**，否则加一条流转就要改三处。
//!
//! 纯函数化的直接收益：25 种组合、加权/等权进度、父任务锁定这三组规则都能在
//! **无数据库**的环境里逐条断言（[`tests`] 里共 15 条）。
//!
//! # 一条容易误解的规则：`need_review`
//!
//! - `need_review = true` ⇒ `DOING` **只能**进 `REVIEW`（`DOING → DONE` 被拒，[`TransitionError::ReviewRequired`]）；
//! - `need_review = false` ⇒ `DOING` **可以**进 `REVIEW`（自愿送验收），**也可以**直接进 `DONE`。
//!
//! 卡面写的是「`need_review=true` 时必须走这里；`need_review=false` 可直接 DONE」——
//! 后半句是「允许」而不是「强制」，所以 `need_review=false` 时 `REVIEW` 仍然开放。
//!
//! # 审批流门闸（批次 D · D3a，报批项）
//!
//! 本批唯一改动既有状态机逻辑的地方：给 `REVIEW` 的两条出边（`→ DONE` / `→ DOING`）
//! 加一道**审批门闸**（见 [`flow_gate_blocked`] / [`check_transition`]）。
//! **`TRANSITIONS` 白名单一行没动** —— 门闸是它之外的一层判定，不是新的流转。
//!
//! 为什么门闸不进白名单：白名单回答「这个 `from → to` 组合存在不存在」（常量，与数据无关），
//! 门闸回答「**这张任务现在**能不能推」（随审批实例状态变化）。塞进常量表会让
//! [`find`] / [`outgoing`]（前端按钮显隐）开始依赖实例数据。
//!
//! 为什么判据是「任务上有没有 `approval_instance_id`」而不是「流程定义开没开审批」：
//! 存量任务可能是「开审批之前」建的，也可能流程后来被停用；以任务自身字段为准，
//! 语义是「这张单确实挂了审批」（架构 §5.2.2）。
//!
//! # 身份维度（批次 D · D3b）
//!
//! 门闸上线后还差两件事，都在本文件里补齐（接线在 `task::handler` 与 `flow::task_bridge`）：
//!
//! 1. **人工路径要知道实例状态**：[`check_transition`] 的第一个参数是 [`ActorKind`]，
//!    调用方（handler）负责查库拿到 `biz_flow_instance.status` 再传进来 —— 本文件仍是纯逻辑。
//! 2. **审批结论要能自动推进任务**：[`ActorKind::FlowCallback`] 是流程引擎的身份，
//!    它绕过门闸与角色（[`callback_transition`]），但**只能在 `REVIEW` 态**推那两条出边。
//!    没有它，「审批通过后任务还停在待验收等人再点一次」会让门闸变成纯减速带。
//!
//! **`TRANSITIONS` 依旧一行没动**：`FlowCallback` 借的是表项里的副作用（进度 / `done_at`），
//! 不是给自己加了一条流转。

use alun::uuid::Uuid;

// ==================== 状态枚举 ====================

/// 任务状态（与 `biz_task.status` 的 5 值 CHECK 逐字一致）。
///
/// **本版不做自定义列**（v2 已批准）：`P1` 的两层状态机（自定义列 → 语义）留给
/// `biz_task.status_semantic`（`V0108` 加的扩展位，本次恒为 `NULL`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskStatus {
    /// 待开始。
    Todo,
    /// 进行中。
    Doing,
    /// 待验收。
    Review,
    /// 已完成。
    Done,
    /// 已取消。
    Cancelled,
}

impl TaskStatus {
    /// 全部 5 态（顺序 = 库表 CHECK 的书写顺序，也是看板列的顺序）。
    pub const ALL: [TaskStatus; 5] = [
        TaskStatus::Todo,
        TaskStatus::Doing,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Cancelled,
    ];

    /// 写入 `biz_task.status` 的字面量。
    pub fn code(self) -> &'static str {
        match self {
            TaskStatus::Todo => "TODO",
            TaskStatus::Doing => "DOING",
            TaskStatus::Review => "REVIEW",
            TaskStatus::Done => "DONE",
            TaskStatus::Cancelled => "CANCELLED",
        }
    }

    /// 解析库里的状态字面量（大小写不敏感，前后空白忽略）。
    ///
    /// # 异常情况
    ///
    /// 不在 5 态之内 → `None`（调用方转 `400 + 63004`）。
    pub fn parse(raw: &str) -> Option<TaskStatus> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "TODO" => Some(TaskStatus::Todo),
            "DOING" => Some(TaskStatus::Doing),
            "REVIEW" => Some(TaskStatus::Review),
            "DONE" => Some(TaskStatus::Done),
            "CANCELLED" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }

    /// 是否终态（`DONE` / `CANCELLED`）。
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Cancelled)
    }

    /// 中文名（错误消息与评论正文用，前端另有 `status-map.ts`）。
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Todo => "待开始",
            TaskStatus::Doing => "进行中",
            TaskStatus::Review => "待验收",
            TaskStatus::Done => "已完成",
            TaskStatus::Cancelled => "已取消",
        }
    }
}

// ==================== 角色位掩码 ====================

/// 角色位：负责人。
pub const ROLE_OWNER: u8 = 1;
/// 角色位：分派人。
pub const ROLE_ASSIGNER: u8 = 2;
/// 角色位：创建人。
pub const ROLE_CREATOR: u8 = 4;
/// 角色位：管理员（超管，`ScopedPrincipal::is_super`）。
pub const ROLE_ADMIN: u8 = 8;

/// 操作人（谁在点这个按钮）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    /// 当前用户。
    pub user_id: Uuid,
    /// 是否管理员（超管）。**只认 `ScopedPrincipal::is_super`**，不认角色名。
    pub is_admin: bool,
}

impl Actor {
    /// 构造操作人。
    pub fn new(user_id: Uuid, is_admin: bool) -> Self {
        Self { user_id, is_admin }
    }
}

/// 谁在推这次流转（架构 §5.2.2：人工请求 vs 流程回调）。
///
/// 两个身份走**同一张白名单**（[`TRANSITIONS`] 一行没动），但判定分支不同：
///
/// | 身份 | 审批门闸 | 白名单 | 角色 | 附加条件 |
/// |---|---|---|---|---|
/// | [`ActorKind::User`] | 查 | 查 | 查 | 查 |
/// | [`ActorKind::FlowCallback`] | **跳过** | 只认 `REVIEW → DONE / DOING` | **跳过** | **跳过** |
///
/// `FlowCallback` 是「审批单已落终态，引擎替人推进任务」的系统身份：它没有 `role_mask`
/// （不是某个人），审批结论本身就替代了人工验收与打回意见，所以角色 / 附加条件一并跳过；
/// 但它**仍然只能在 `REVIEW` 态**里推（见 [`callback_transition`]）—— 否则「审批通过」
/// 会变成一把能跨状态跳级的万能钥匙。
///
/// **红线**：`FlowCallback` 只能由流程回调（`flow::task_bridge::on_instance_settled`）使用。
/// HTTP handler 一律传 [`ActorKind::User`]，**不许从请求体 / 请求头里读这个值**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    /// 人类（HTTP 请求，走 `#[alun::permission]` 的那条链）。
    User,
    /// 流程引擎回调（审批单落终态后自动推进任务）。
    FlowCallback,
}

impl ActorKind {
    /// 是否流程回调身份。
    pub fn is_callback(self) -> bool {
        matches!(self, ActorKind::FlowCallback)
    }
}

/// 任务上的三个「人」列（`biz_task` 的 `owner_id` / `creator_id` / `assigner_id`）。
///
/// **协作人（`biz_task_member`）不在这里**：协作人看得见但推不动状态（T1 的口径：
/// 可见性 ≠ 写权限）。把它放进来会让「拉个人进来帮我看进度」变成「他能替我交活」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskPeople {
    /// 负责人。
    pub owner_id: Uuid,
    /// 创建人。
    pub creator_id: Uuid,
    /// 分派人（自建任务为空）。
    pub assigner_id: Option<Uuid>,
}

impl TaskPeople {
    /// 算出操作人在本条任务上的角色位掩码。
    ///
    /// 一个人可以同时是多个角色（自己建、自己干、自己验收），掩码取并集。
    /// 管理员位**独立叠加**：超管不是负责人也能推验收。
    pub fn role_mask(&self, actor: &Actor) -> u8 {
        let mut mask = 0;
        if self.owner_id == actor.user_id {
            mask |= ROLE_OWNER;
        }
        if self.creator_id == actor.user_id {
            mask |= ROLE_CREATOR;
        }
        if self.assigner_id == Some(actor.user_id) {
            mask |= ROLE_ASSIGNER;
        }
        if actor.is_admin {
            mask |= ROLE_ADMIN;
        }
        mask
    }

    /// 角色掩码 → 中文描述（只用于错误消息，让人一眼看出「我是什么身份」）。
    pub fn describe(mask: u8) -> String {
        let mut roles = Vec::new();
        if mask & ROLE_OWNER != 0 {
            roles.push("负责人");
        }
        if mask & ROLE_CREATOR != 0 {
            roles.push("创建人");
        }
        if mask & ROLE_ASSIGNER != 0 {
            roles.push("分派人");
        }
        if mask & ROLE_ADMIN != 0 {
            roles.push("管理员");
        }
        if roles.is_empty() {
            return "无（协作人 / 关注者 / 无关用户）".to_string();
        }
        roles.join(" / ")
    }
}

// ==================== 白名单流转表 ====================

/// `REVIEW → DOING`（打回）原因的最少字符数（卡面「≥5 字」）。
pub const REJECT_REASON_MIN_CHARS: usize = 5;

/// 进度副作用（**只对无子任务的任务生效**；有子任务的进度由子任务汇总）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEffect {
    /// 不动（取消类流转：冻结当时值）。
    Keep,
    /// 只抬下限：`max(当前, n)`。
    AtLeast(i16),
    /// 归位到 n（返工 / 重开 / 恢复）。
    Reset(i16),
    /// 拉满 / 直接置为 n。
    Force(i16),
}

impl ProgressEffect {
    /// 按副作用算出新进度；`None` = 不要改 `progress` 列。
    pub fn apply(self, current: i16) -> Option<i16> {
        match self {
            ProgressEffect::Keep => None,
            ProgressEffect::AtLeast(floor) => Some(current.max(floor)),
            ProgressEffect::Reset(value) | ProgressEffect::Force(value) => {
                Some(clamp_progress(value))
            }
        }
    }
}

/// 完成时间的副作用（`biz_task.done_at`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DoneAtEffect {
    /// 不动。
    #[default]
    Keep,
    /// 写 `now()`。
    SetNow,
    /// 清空（重开）。
    Clear,
}

/// 一条白名单流转。
#[derive(Debug, Clone, Copy)]
pub struct Transition {
    /// 起点状态。
    pub from: TaskStatus,
    /// 终点状态。
    pub to: TaskStatus,
    /// 允许的角色位掩码（[`TaskPeople::role_mask`] 与之取交集，非空才放行）。
    pub actors: u8,
    /// 无子任务时的进度副作用。
    pub progress: ProgressEffect,
    /// `done_at` 副作用。
    pub done_at: DoneAtEffect,
    /// 是否必须填打回原因（`REVIEW → DOING`）。
    pub requires_reject_reason: bool,
    /// 是否仅 `need_review = false` 时可用（`DOING → DONE`）。
    pub requires_no_review: bool,
    /// 中文说明（错误消息 / 审计留痕用）。
    pub label: &'static str,
}

impl Transition {
    /// 这个流转对给定角色是否可见（用于前端按钮显隐 / 文档生成）。
    pub fn allows(&self, role_mask: u8) -> bool {
        role_mask & self.actors != 0
    }
}

/// **唯一**的合法流转清单（5×5=25 里只放这 10 条，其余一律拒绝）。
///
/// | # | from → to | 谁能做 | 附加条件 |
/// |---|---|---|---|
/// | 1 | `TODO → DOING` | 负责人 / 分派人 / 创建人 / 管理员 | 前置依赖全 DONE（T4 接线，见 [`Transition::label`] 的 H-? 标记） |
/// | 2 | `DOING → REVIEW` | 负责人 | `need_review=false` 时也可主动送验收 |
/// | 3 | `DOING → DONE` | 负责人 / 创建人 | 仅 `need_review=false` |
/// | 4 | `REVIEW → DONE` | 创建人 / 分派人 / 管理员（验收人） | 负责人自己不算验收人 |
/// | 5 | `REVIEW → DOING` | 验收人（打回） | 必填打回原因（≥5 字） |
/// | 6 | `DONE → DOING` | 创建人 / 管理员（重开） | 进度归位到 10% |
/// | 7 | `TODO → CANCELLED` | 负责人 / 创建人 / 分派人 / 管理员 | 进度冻结 |
/// | 8 | `DOING → CANCELLED` | 同上 | 进度冻结 |
/// | 9 | `REVIEW → CANCELLED` | 同上 | 进度冻结 |
/// | 10 | `CANCELLED → TODO` | 创建人 / 管理员（恢复） | 进度归 0 |
///
/// **`DONE` 不能直接取消**（必须先重开或走别的流程）——这是表里第 7/8/9 条
/// 只写「非 DONE」的原因，也是 [`TransitionError::IllegalPair`] 里最容易被误放行的一条。
pub const TRANSITIONS: [Transition; 10] = [
    Transition {
        from: TaskStatus::Todo,
        to: TaskStatus::Doing,
        actors: ROLE_OWNER | ROLE_ASSIGNER | ROLE_CREATOR | ROLE_ADMIN,
        progress: ProgressEffect::AtLeast(10),
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "开始执行（前置依赖由 T4 校验：未完成前置 → 409）",
    },
    Transition {
        from: TaskStatus::Doing,
        to: TaskStatus::Review,
        actors: ROLE_OWNER,
        progress: ProgressEffect::Force(100),
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "提交验收",
    },
    Transition {
        from: TaskStatus::Doing,
        to: TaskStatus::Done,
        actors: ROLE_OWNER | ROLE_CREATOR,
        progress: ProgressEffect::Force(100),
        done_at: DoneAtEffect::SetNow,
        requires_reject_reason: false,
        requires_no_review: true,
        label: "直接完成（免验收任务）",
    },
    Transition {
        from: TaskStatus::Review,
        to: TaskStatus::Done,
        actors: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
        progress: ProgressEffect::Force(100),
        done_at: DoneAtEffect::SetNow,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "验收通过",
    },
    Transition {
        from: TaskStatus::Review,
        to: TaskStatus::Doing,
        actors: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
        progress: ProgressEffect::Reset(10),
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: true,
        requires_no_review: false,
        label: "验收打回（返工）",
    },
    Transition {
        from: TaskStatus::Done,
        to: TaskStatus::Doing,
        actors: ROLE_CREATOR | ROLE_ADMIN,
        progress: ProgressEffect::Reset(10),
        done_at: DoneAtEffect::Clear,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "重开（进度回退、清完成时间）",
    },
    Transition {
        from: TaskStatus::Todo,
        to: TaskStatus::Cancelled,
        actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
        progress: ProgressEffect::Keep,
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "取消（H-REL 解除下游依赖在 T4 接入）",
    },
    Transition {
        from: TaskStatus::Doing,
        to: TaskStatus::Cancelled,
        actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
        progress: ProgressEffect::Keep,
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "取消（H-REL 解除下游依赖在 T4 接入）",
    },
    Transition {
        from: TaskStatus::Review,
        to: TaskStatus::Cancelled,
        actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
        progress: ProgressEffect::Keep,
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "取消（H-REL 解除下游依赖在 T4 接入）",
    },
    Transition {
        from: TaskStatus::Cancelled,
        to: TaskStatus::Todo,
        actors: ROLE_CREATOR | ROLE_ADMIN,
        progress: ProgressEffect::Reset(0),
        done_at: DoneAtEffect::Keep,
        requires_reject_reason: false,
        requires_no_review: false,
        label: "恢复为待开始",
    },
];

/// 查表：合法则返回登记项，非法返回 `None`（**唯一判据**）。
pub fn find(from: TaskStatus, to: TaskStatus) -> Option<&'static Transition> {
    TRANSITIONS
        .iter()
        .find(|transition| transition.from == from && transition.to == to)
}

/// 某个状态的出边（前端画「下一步」按钮 / 单测断言用）。
pub fn outgoing(from: TaskStatus) -> Vec<&'static Transition> {
    TRANSITIONS
        .iter()
        .filter(|transition| transition.from == from)
        .collect()
}

// ==================== 授权判定 ====================

/// 流转被拒的原因（handler 据此映射 HTTP 状态码与业务码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// 起终点相同（`DOING → DOING`）：状态机里没有自环。
    SameStatus,
    /// 表外组合（含 `TODO → DONE` 跳级、`DONE → CANCELLED` 直取、`REVIEW → TODO` 回退）。
    IllegalPair,
    /// 该角色没有推这个流转的权限（`REVIEW → DONE` 由负责人自己做）。
    Forbidden {
        /// 本流转允许的角色位。
        allowed: u8,
        /// 操作人在本条任务上的角色位。
        actual: u8,
    },
    /// `need_review = true` 时不能跳过验收直接完成。
    ReviewRequired,
    /// 打回没填原因 / 原因太短。
    RejectReasonRequired {
        /// 已有字符数（去首尾空白后按 `char` 计）。
        actual_chars: usize,
    },
}

impl TransitionError {
    /// 面向用户的提示文案（**要说清下一步怎么做**，不能只写「非法」）。
    pub fn message(&self) -> String {
        match self {
            TransitionError::SameStatus => "任务已处于该状态，无需流转".to_string(),
            TransitionError::IllegalPair => {
                "该状态流转不在允许的白名单内（TODO → DOING → REVIEW → DONE，任意非终态可取消，CANCELLED 可恢复为 TODO）".to_string()
            }
            TransitionError::Forbidden { allowed, actual } => format!(
                "当前身份（{}）无权执行该流转，允许的身份：{}",
                TaskPeople::describe(*actual),
                TaskPeople::describe(*allowed)
            ),
            TransitionError::ReviewRequired => {
                "该任务需要验收（need_review=true），必须先提交验收（DOING → REVIEW），由验收人确认后才能完成".to_string()
            }
            TransitionError::RejectReasonRequired { actual_chars } => format!(
                "打回必须填写原因（至少 {REJECT_REASON_MIN_CHARS} 个字，当前 {actual_chars} 个字）"
            ),
        }
    }
}

/// 判定一次流转是否被允许（**白名单 → 角色 → 附加条件** 三段式，顺序不可换）。
///
/// # 参数
///
/// - `from` / `to`：起点（库里的现值）与目标；
/// - `role_mask`：操作人在本条任务上的角色位（[`TaskPeople::role_mask`]）；
/// - `need_review`：任务的验收开关；
/// - `reason`：本次流转附带的原因（打回必填）。
///
/// # 返回值
///
/// 命中的 [`Transition`]（含进度 / 完成时间副作用）。
///
/// # 异常情况
///
/// 见 [`TransitionError`]。
pub fn authorize(
    from: TaskStatus,
    to: TaskStatus,
    role_mask: u8,
    need_review: bool,
    reason: Option<&str>,
) -> std::result::Result<&'static Transition, TransitionError> {
    // ① 自环先挡：`DOING → DOING` 不在表里，但单独给一条更准确的提示
    if from == to {
        return Err(TransitionError::SameStatus);
    }
    // ② 白名单：表外组合一律拒绝（**这一步不许有例外分支**）
    let transition = find(from, to).ok_or(TransitionError::IllegalPair)?;
    // ③ 角色
    if !transition.allows(role_mask) {
        return Err(TransitionError::Forbidden {
            allowed: transition.actors,
            actual: role_mask,
        });
    }
    // ④ 附加条件：验收开关
    if transition.requires_no_review && need_review {
        return Err(TransitionError::ReviewRequired);
    }
    // ⑤ 附加条件：打回原因
    if transition.requires_reject_reason {
        let chars = reason.map(count_chars).unwrap_or(0);
        if chars < REJECT_REASON_MIN_CHARS {
            return Err(TransitionError::RejectReasonRequired {
                actual_chars: chars,
            });
        }
    }
    Ok(transition)
}

/// 去首尾空白后的字符数（按 `char` 计：中文 5 个字 = 5，不是 15 字节）。
fn count_chars(raw: &str) -> usize {
    raw.trim().chars().count()
}

// ==================== 审批流门闸（批次 D · D3a） ====================

/// 审批实例「进行中」的状态字面量（`biz_flow_instance.status`，与 flow 模块 CHECK 同字面量）。
pub const APPROVAL_INSTANCE_PENDING: &str = "PENDING";

/// 门闸拦截时给用户看的文案（handler 映射 `409 + 65030` 时直接用）。
///
/// 卡面定稿文案：先告诉用户「这事不是你点一下能过的」，再告诉他去哪儿（审批单）。
pub const FLOW_GATE_BLOCKED_MESSAGE: &str = "任务正在审批中，请等待审批结果";

/// 该流转是否属于「被审批单接管」的两条（`REVIEW → DONE` / `REVIEW → DOING`）。
///
/// 只锁这两条：`REVIEW → CANCELLED` 是「审批中也要能刹车」（取消是人工权限，不该被审批单锁死），
/// 其它 `from` 与本门闸无关。
pub fn gate_applies(from: TaskStatus, to: TaskStatus) -> bool {
    from == TaskStatus::Review && matches!(to, TaskStatus::Done | TaskStatus::Doing)
}

/// 任务是否被审批门闸拦截（`REVIEW` 出边 且 审批单 `PENDING`）—— **纯函数，不查库**。
///
/// # 参数（与卡面 `task: &TaskRow` 的差异）
///
/// 本模块是**纯逻辑**（模块头第一行：不碰数据库），而任务行结构体在库里叫
/// `task::model::TaskDetailRow`、且状态列是 `String`。所以入参拆成三个原始值：
/// 已 `parse` 的当前状态 / 目标状态 / 任务上的 `approval_instance_id` + 实例状态。
/// 调用方（handler）负责查库，本函数负责判定 —— 这正是「纯函数便于单测」的形态。
///
/// # 判据
///
/// | 条件 | 结果 |
/// |---|---|
/// | 不是 `REVIEW → DONE` / `REVIEW → DOING` | `false`（不受影响） |
/// | 任务没挂审批单（`approval_instance_id = None`） | `false`（老逻辑照跑） |
/// | 挂了审批单且实例状态 `PENDING`（大小写 / 空白不敏感） | `true`（拦） |
/// | 挂了审批单、实例 `APPROVED` / `REJECTED` / 其它 / 读不到 | `false`（放行，见下） |
///
/// 「读不到实例状态」（`None`）**不拦**：这张任务的审批单可能已被撤回或数据不一致，
/// 让任务永久卡在 `REVIEW` 比放行更坏；实例合法性归 flow 引擎（65024
/// `FLOW_INSTANCE_NOT_PENDING` 等）自己判定，本门闸只管「审批正在进行中」这一件事。
pub fn flow_gate_blocked(
    from: TaskStatus,
    to: TaskStatus,
    approval_instance_id: Option<Uuid>,
    instance_status: Option<&str>,
) -> bool {
    if !gate_applies(from, to) {
        return false;
    }
    if approval_instance_id.is_none() {
        return false;
    }
    instance_status.is_some_and(|status| {
        status
            .trim()
            .eq_ignore_ascii_case(APPROVAL_INSTANCE_PENDING)
    })
}

/// 门闸 + 状态机的联合判定结果。
///
/// 单独一个枚举而不是往 [`TransitionError`] 里加变体：`TransitionError` 是「状态机说不行」，
/// 与「审批还没结束」是两类事（HTTP 码与业务码都不同：`409/65030` vs `400/403/409`）。
/// 混进一个变体会让既有 `handler::transition_error` 的三档映射多一个例外分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionCheckError {
    /// 审批单 `PENDING`，人工路径一律拦 → `409 + 65030`（`FLOW_GATE_BLOCKED_MESSAGE`）。
    ApprovalPending,
    /// 状态机自身的拒绝（白名单 / 角色 / 附加条件）。
    Transition(TransitionError),
}

impl TransitionCheckError {
    /// 面向用户的文案（门闸用自己的定稿文案，其余转发状态机的）。
    pub fn message(&self) -> String {
        match self {
            TransitionCheckError::ApprovalPending => FLOW_GATE_BLOCKED_MESSAGE.to_string(),
            TransitionCheckError::Transition(error) => error.message(),
        }
    }

    /// 是否被**审批门闸**拦截（handler 用它区分 65030 与 63010/63011/63013）。
    pub fn is_gate(&self) -> bool {
        matches!(self, TransitionCheckError::ApprovalPending)
    }
}

/// 带审批门闸的流转判定：**门闸先于白名单 / 角色**（一处判定，不在 handler 里各写一遍）。
///
/// # 为什么门闸排在最前面
///
/// 门闸回答的是「这张任务的审批还没结束」——**与你是谁无关**。所以创建人、管理员、
/// 负责人、甚至协作人点下去得到的都是同一个 `409 + 65030`，而不是「有的人 409、
/// 有的人 403」：此时正确的下一步只有「去审批单里处理/等人批」，换个人来做结论一样。
/// （因此本函数的门闸分支不看 `role_mask`；`REVIEW → DOING` 即便操作人角色不够，
/// 在审批中也先报 65030。）
///
/// 参数与 [`authorize`] 一致，另外两个是门闸输入（见 [`flow_gate_blocked`]），第一个是身份。
///
/// # 身份分支（D3b）
///
/// - [`ActorKind::User`]：门闸 → 白名单 → 角色 → 附加条件（上面那一整套）；
/// - [`ActorKind::FlowCallback`]：走 [`callback_transition`]，**门闸与角色都不看**，
///   但仍要求 `from = REVIEW` 且 `to ∈ {DONE, DOING}`。
///
/// # 异常情况
///
/// - [`TransitionCheckError::ApprovalPending`] → `409 + 65030`；
/// - [`TransitionCheckError::Transition`] → 沿用 `authorize` 的三档（`400/63013`、`403/63011`、`409/63010`）。
pub fn check_transition(
    actor: ActorKind,
    from: TaskStatus,
    to: TaskStatus,
    role_mask: u8,
    need_review: bool,
    reason: Option<&str>,
    approval_instance_id: Option<Uuid>,
    instance_status: Option<&str>,
) -> std::result::Result<&'static Transition, TransitionCheckError> {
    // ⓪ 流程回调：系统身份，绕过门闸与角色判定（但只能在 REVIEW 态推两条出边）
    if actor.is_callback() {
        return callback_transition(from, to).map_err(TransitionCheckError::Transition);
    }
    // ① 审批门闸：先于白名单 / 角色（人工路径）
    if flow_gate_blocked(from, to, approval_instance_id, instance_status) {
        return Err(TransitionCheckError::ApprovalPending);
    }
    // ② 白名单 + 角色 + 附加条件
    authorize(from, to, role_mask, need_review, reason).map_err(TransitionCheckError::Transition)
}

/// `FlowCallback` 身份的流转判定：**绕过门闸 / 角色 / 附加条件**，
/// 但**仍受「必须在 REVIEW 态」约束**。
///
/// # 允许的两条（其余一律 [`TransitionError::IllegalPair`]）
///
/// | from → to | 语义 | 审批意见的落点 |
/// |---|---|---|
/// | `REVIEW → DONE` | 审批通过 → 任务完成 | 评论正文 |
/// | `REVIEW → DOING` | 审批驳回 → 任务返工 | `reject_reason` |
///
/// # 为什么还要 `find(from, to)` 查表
///
/// 进度副作用（`Force(100)` / `Reset(10)`）、`done_at` 副作用与中文文案都挂在表项上。
/// 回调**只借表项、不借它的角色判定**，这样「审批通过 = 进度 100 + 写完成时间」与人工
/// 点「验收通过」是同一份口径；直接 `if` 造一条 Transition 就会分叉出第二份副作用规则。
///
/// # 为什么仍然必须 `from = REVIEW`
///
/// 审批结论只对「等待验收」有意义。放开 `from` 就等于让一条审批通过把任务从 `TODO`
/// 直接推到 `DONE`——那是跳级，白名单的 25 选 10 就白定了。
pub fn callback_transition(
    from: TaskStatus,
    to: TaskStatus,
) -> std::result::Result<&'static Transition, TransitionError> {
    if from != TaskStatus::Review {
        return Err(TransitionError::IllegalPair);
    }
    if !matches!(to, TaskStatus::Done | TaskStatus::Doing) {
        return Err(TransitionError::IllegalPair);
    }
    find(from, to).ok_or(TransitionError::IllegalPair)
}

// ==================== 进度算法 ====================

/// 一个子任务的进度快照（聚合只需要这 4 个值）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChildSnapshot {
    /// 子任务状态。
    pub status: TaskStatus,
    /// 子任务进度 0~100。
    pub progress: i16,
    /// 子任务预估工时（小时，可空）。
    pub estimate_hours: Option<f64>,
}

/// 把任意整数夹到 0~100（`SMALLINT` + CHECK 约束的软护栏）。
pub fn clamp_progress(value: i16) -> i16 {
    value.clamp(0, 100)
}

/// 父任务进度聚合（**写路径**调用：子任务变更时重算父任务）。
///
/// # 口径（核心设计 §三 + 架构 D4/D5）
///
/// 1. 没有子任务 → 返回 `None`（调用方：父任务由手动维护，不走汇总）；
/// 2. **子任务全部 DONE → 100**（架构 D5：只到 100%，**绝不**把父任务也置为 DONE）；
/// 3. 所有子任务都有工时（`estimate_hours` 全部非空且 > 0）→
///    `Σ(工时 × 进度) / Σ(工时)`（**工时加权**，架构 D4）；
/// 4. 工时不全（有任何一个为空 / 为 0）→ **等权平均** `Σ进度 / 子任务数`。
///
/// # 为什么工时「不全」时整组退化成等权，而不是「空工时按 0 权重」
///
/// 按 0 权重会让空工时的子任务**完全不影响**父任务 —— 一个 7 小时的活拆出来的子任务
/// 忘了填工时，父任务就会被另外几条小任务拉满，「看起来做完了」是最坏的结果。
/// 等权是更保守的降级：每条子任务都算数，只是不再按体量折算。
///
/// # 已取消的子任务
///
/// **计入聚合**（取其冻结的进度）：取消代表「这部分不做完」而不是「这部分凭空完成」，
/// 从分母里剔掉会让父任务凭空涨进度。子任务全 `DONE` 的捷径只认 `DONE`，
/// 所以「有子任务被取消」的父任务不会因为这条捷径被拉满 —— 需要人来决定收尾方式。
///
/// # 异常情况
///
/// 不返回错误：所有输入都在 0~100 与有限工时内（DB 的 CHECK 保证），
/// 结果统一夹回 0~100 并四舍五入取整。
pub fn aggregate_progress(children: &[ChildSnapshot]) -> Option<i16> {
    if children.is_empty() {
        return None;
    }
    // D5：子任务全 DONE → 父任务进度 100（状态不动）
    if children
        .iter()
        .all(|child| child.status == TaskStatus::Done)
    {
        return Some(100);
    }
    let weighted = children
        .iter()
        .all(|child| child.estimate_hours.is_some_and(|hours| hours > 0.0));
    let value = if weighted {
        let total_hours: f64 = children
            .iter()
            .filter_map(|child| child.estimate_hours)
            .sum();
        if total_hours > 0.0 {
            children
                .iter()
                .map(|child| child.estimate_hours.unwrap_or(0.0) * f64::from(child.progress))
                .sum::<f64>()
                / total_hours
        } else {
            equal_weight_average(children)
        }
    } else {
        equal_weight_average(children)
    };
    Some(clamp_progress(value.round() as i16))
}

/// 等权平均（`Σ进度 / 条数`）。
fn equal_weight_average(children: &[ChildSnapshot]) -> f64 {
    let sum: i64 = children.iter().map(|child| i64::from(child.progress)).sum();
    sum as f64 / children.len() as f64
}

/// 未完成子任务数（`TODO` / `DOING` / `REVIEW`）。
///
/// 用于两条闸：① 进入 `DONE` 前（设计 §三「父任务 DONE 前置检查」）；
/// ② 父任务「待汇总」通知的触发条件（降为 0 且全部 DONE）。
pub fn open_children(children: &[ChildSnapshot]) -> Vec<&ChildSnapshot> {
    children
        .iter()
        .filter(|child| !child.status.is_terminal())
        .collect()
}

/// 父任务进度是否被「子任务汇总」锁定（有子任务就不许手改）。
///
/// 红线：`PATCH /progress` 命中这个条件必须 `409 + 63012`，**不许悄悄改**。
pub fn progress_locked(children: &[ChildSnapshot]) -> bool {
    !children.is_empty()
}

/// 父任务是否「可结项」（全部子任务 DONE 且进度 100）—— 只用于通知文案，不自动改状态。
pub fn ready_to_close(children: &[ChildSnapshot]) -> bool {
    !children.is_empty()
        && children
            .iter()
            .all(|child| child.status == TaskStatus::Done)
}

/// 聚合口径的中文名（写进 `PROGRESS_AUTO` 评论，事后核对「80% 是怎么算出来的」）。
pub fn aggregation_label(children: &[ChildSnapshot]) -> &'static str {
    if children.is_empty() {
        return "无子任务";
    }
    if ready_to_close(children) {
        return "子任务全部完成";
    }
    if children
        .iter()
        .all(|child| child.estimate_hours.is_some_and(|hours| hours > 0.0))
    {
        "工时加权"
    } else {
        "等权平均"
    }
}

// ==================== 单元测试（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn who(owner: u128, creator: u128, assigner: Option<u128>) -> TaskPeople {
        TaskPeople {
            owner_id: Uuid::from_u128(owner),
            creator_id: Uuid::from_u128(creator),
            assigner_id: assigner.map(Uuid::from_u128),
        }
    }

    fn actor(id: u128, is_admin: bool) -> Actor {
        Actor::new(Uuid::from_u128(id), is_admin)
    }

    fn child(status: TaskStatus, progress: i16, hours: Option<f64>) -> ChildSnapshot {
        ChildSnapshot {
            status,
            progress,
            estimate_hours: hours,
        }
    }

    // ---------- 状态机：25 种组合的完整矩阵 ----------

    #[test]
    fn whitelist_covers_exactly_the_ten_allowed_pairs() {
        // 期望放行的 10 对（与卡面表格逐条对应）
        let allowed = [
            (TaskStatus::Todo, TaskStatus::Doing),
            (TaskStatus::Doing, TaskStatus::Review),
            (TaskStatus::Doing, TaskStatus::Done),
            (TaskStatus::Review, TaskStatus::Done),
            (TaskStatus::Review, TaskStatus::Doing),
            (TaskStatus::Done, TaskStatus::Doing),
            (TaskStatus::Todo, TaskStatus::Cancelled),
            (TaskStatus::Doing, TaskStatus::Cancelled),
            (TaskStatus::Review, TaskStatus::Cancelled),
            (TaskStatus::Cancelled, TaskStatus::Todo),
        ];
        assert_eq!(TRANSITIONS.len(), 10, "白名单必须是 10 条");
        assert_eq!(allowed.len(), 10);

        // 5 × 5 = 25 种组合逐一断言：表里有的必须命中，表里没有的必须拒绝
        let mut hit = 0;
        for from in TaskStatus::ALL {
            for to in TaskStatus::ALL {
                let expected = allowed.contains(&(from, to));
                let actual = find(from, to).is_some();
                assert_eq!(
                    actual,
                    expected,
                    "{} → {} 的白名单判定错误（期望 {}，实际 {}）",
                    from.code(),
                    to.code(),
                    expected,
                    actual
                );
                if actual {
                    hit += 1;
                }
            }
        }
        assert_eq!(hit, 10, "25 种组合里必须恰好放行 10 种");
        // 反向确认：15 种非法组合全部拒绝
        assert_eq!(25 - hit, 15, "非法组合必须是 15 种");
    }

    #[test]
    fn illegal_pairs_are_rejected_with_illegal_pair() {
        // 红线里的三条典型非法流转
        for (from, to) in [
            (TaskStatus::Todo, TaskStatus::Done),      // 跳级
            (TaskStatus::Done, TaskStatus::Cancelled), // 终态不能直接取消
            (TaskStatus::Review, TaskStatus::Todo),    // 不能退回待开始
        ] {
            let error = authorize(from, to, 0b1111, false, Some("随便写五个字"))
                .expect_err("非法流转必须被拒");
            assert_eq!(error, TransitionError::IllegalPair);
        }
        // 自环单独一条（提示更准确）
        for status in TaskStatus::ALL {
            let error =
                authorize(status, status, 0b1111, false, None).expect_err("同状态流转必须被拒");
            assert_eq!(error, TransitionError::SameStatus);
        }
    }

    #[test]
    fn no_transition_targets_itself_and_pairs_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for transition in TRANSITIONS {
            assert_ne!(transition.from, transition.to, "白名单里不许有自环");
            assert!(
                transition.actors != 0,
                "{} 的角色掩码不能为空",
                transition.label
            );
            assert!(
                seen.insert((transition.from, transition.to)),
                "{} → {} 重复登记",
                transition.from.code(),
                transition.to.code()
            );
            // 每条 from 都必须是当前存在的 5 态之一
            assert!(TaskStatus::ALL.contains(&transition.from));
            assert!(TaskStatus::ALL.contains(&transition.to));
        }
        assert_eq!(seen.len(), 10);
    }

    // ---------- 状态机：权限 ----------

    #[test]
    fn review_to_done_needs_acceptance_right_not_just_ownership() {
        // 负责人 ≠ 创建人 ≠ 分派人：负责人自己点「验收通过」必须被拒
        let people = who(1, 2, Some(3));
        let owner_mask = people.role_mask(&actor(1, false));
        assert_eq!(owner_mask, ROLE_OWNER);
        let error = authorize(TaskStatus::Review, TaskStatus::Done, owner_mask, true, None)
            .expect_err("负责人没有验收权");
        assert_eq!(
            error,
            TransitionError::Forbidden {
                allowed: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
                actual: ROLE_OWNER,
            }
        );
        // 创建人 / 分派人 / 管理员都可以
        for mask in [ROLE_CREATOR, ROLE_ASSIGNER, ROLE_ADMIN] {
            assert!(authorize(TaskStatus::Review, TaskStatus::Done, mask, true, None).is_ok());
        }
        // 协作人（无角色位）一律拒绝
        assert!(matches!(
            authorize(TaskStatus::Review, TaskStatus::Done, 0, true, None),
            Err(TransitionError::Forbidden { .. })
        ));
    }

    #[test]
    fn todo_to_doing_allows_four_roles_and_rejects_none() {
        for mask in [ROLE_OWNER, ROLE_ASSIGNER, ROLE_CREATOR, ROLE_ADMIN] {
            assert!(
                authorize(TaskStatus::Todo, TaskStatus::Doing, mask, false, None).is_ok(),
                "掩码 {mask} 应可开始执行"
            );
        }
        assert!(matches!(
            authorize(TaskStatus::Todo, TaskStatus::Doing, 0, false, None),
            Err(TransitionError::Forbidden { .. })
        ));
    }

    #[test]
    fn reopen_and_restore_are_creator_or_admin_only() {
        // DONE → DOING：负责人不行（自己不能给自己重开）
        assert!(matches!(
            authorize(TaskStatus::Done, TaskStatus::Doing, ROLE_OWNER, false, None),
            Err(TransitionError::Forbidden { .. })
        ));
        assert!(
            authorize(
                TaskStatus::Done,
                TaskStatus::Doing,
                ROLE_CREATOR,
                false,
                None
            )
            .is_ok()
        );
        assert!(authorize(TaskStatus::Done, TaskStatus::Doing, ROLE_ADMIN, false, None).is_ok());
        // CANCELLED → TODO：同样只认创建人 / 管理员
        assert!(matches!(
            authorize(
                TaskStatus::Cancelled,
                TaskStatus::Todo,
                ROLE_OWNER,
                false,
                None
            ),
            Err(TransitionError::Forbidden { .. })
        ));
        assert!(
            authorize(
                TaskStatus::Cancelled,
                TaskStatus::Todo,
                ROLE_ADMIN,
                false,
                None
            )
            .is_ok()
        );
    }

    // ---------- 状态机：need_review ----------

    #[test]
    fn need_review_blocks_direct_done_but_keeps_review_open() {
        // need_review = true ⇒ DOING → DONE 被拒
        let error = authorize(TaskStatus::Doing, TaskStatus::Done, ROLE_OWNER, true, None)
            .expect_err("需验收的任务不能跳过 REVIEW");
        assert_eq!(error, TransitionError::ReviewRequired);
        // need_review = true ⇒ DOING → REVIEW 正常
        assert!(
            authorize(
                TaskStatus::Doing,
                TaskStatus::Review,
                ROLE_OWNER,
                true,
                None
            )
            .is_ok()
        );
        // need_review = false ⇒ 两条都通
        assert!(authorize(TaskStatus::Doing, TaskStatus::Done, ROLE_OWNER, false, None).is_ok());
        assert!(
            authorize(
                TaskStatus::Doing,
                TaskStatus::Review,
                ROLE_OWNER,
                false,
                None
            )
            .is_ok()
        );
    }

    // ---------- 状态机：打回原因 ----------

    #[test]
    fn reject_requires_reason_of_at_least_five_chars() {
        let mask = ROLE_CREATOR;
        for (reason, expected_chars) in [
            (None, 0usize),
            (Some(""), 0usize),
            (Some("   "), 0usize),
            (Some("太差"), 2usize),
            (Some("四个字符"), 4usize),
        ] {
            let error = authorize(TaskStatus::Review, TaskStatus::Doing, mask, true, reason)
                .expect_err("原因不足必须被拒");
            assert_eq!(
                error,
                TransitionError::RejectReasonRequired {
                    actual_chars: expected_chars
                }
            );
        }
        // 恰好 5 个字（中文按 char 计）通过
        assert!(
            authorize(
                TaskStatus::Review,
                TaskStatus::Doing,
                mask,
                true,
                Some("五个字符啊")
            )
            .is_ok()
        );
        // 前后空白不算
        assert!(
            authorize(
                TaskStatus::Review,
                TaskStatus::Doing,
                mask,
                true,
                Some("  需要补测试用例  ")
            )
            .is_ok()
        );
        // 其它流转不要求原因
        assert!(
            authorize(
                TaskStatus::Doing,
                TaskStatus::Review,
                ROLE_OWNER,
                true,
                None
            )
            .is_ok()
        );
    }

    // ---------- 状态机：副作用 ----------

    #[test]
    fn progress_effects_follow_the_transition() {
        let mut cases = 0;
        for transition in TRANSITIONS {
            let applied = transition.progress.apply(50);
            match transition.to {
                // 进入 REVIEW / DONE：无子任务时拉满
                TaskStatus::Review | TaskStatus::Done => {
                    assert_eq!(applied, Some(100), "{} 应拉满进度", transition.label);
                    cases += 1;
                }
                // 进入 DOING：TODO → DOING 只抬下限（保留已有手动进度），
                // 从 REVIEW / DONE 回来是返工 / 重开，归位到 10
                TaskStatus::Doing => {
                    let expected = if transition.from == TaskStatus::Todo {
                        Some(50)
                    } else {
                        Some(10)
                    };
                    assert_eq!(applied, expected, "{} 的进度副作用不对", transition.label);
                    cases += 1;
                }
                // 取消：冻结
                TaskStatus::Cancelled => {
                    assert_eq!(applied, None, "取消应冻结进度");
                    cases += 1;
                }
                // 恢复：归 0
                TaskStatus::Todo => {
                    assert_eq!(applied, Some(0), "恢复应归 0");
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 10);
        // AtLeast 只抬不降
        assert_eq!(ProgressEffect::AtLeast(10).apply(0), Some(10));
        assert_eq!(ProgressEffect::AtLeast(10).apply(60), Some(60));
    }

    #[test]
    fn done_at_effects_are_set_on_done_and_cleared_on_reopen() {
        for transition in TRANSITIONS {
            let expected = match (transition.from, transition.to) {
                (_, TaskStatus::Done) => DoneAtEffect::SetNow,
                (TaskStatus::Done, _) => DoneAtEffect::Clear,
                _ => DoneAtEffect::Keep,
            };
            assert_eq!(
                transition.done_at, expected,
                "{} 的 done_at 副作用不对",
                transition.label
            );
        }
    }

    #[test]
    fn role_mask_unions_and_admin_is_independent() {
        let people = who(1, 1, Some(2));
        // 负责人 = 创建人（自建自干）→ 两位并存
        assert_eq!(
            people.role_mask(&actor(1, false)),
            ROLE_OWNER | ROLE_CREATOR
        );
        // 分派人
        assert_eq!(people.role_mask(&actor(2, false)), ROLE_ASSIGNER);
        // 无关用户：0
        assert_eq!(people.role_mask(&actor(9, false)), 0);
        // 超管：管理员位独立叠加（不是负责人也能推验收）
        assert_eq!(people.role_mask(&actor(9, true)), ROLE_ADMIN);
        assert_eq!(
            people.role_mask(&actor(1, true)),
            ROLE_OWNER | ROLE_CREATOR | ROLE_ADMIN
        );
        assert_eq!(TaskPeople::describe(0), "无（协作人 / 关注者 / 无关用户）");
        assert_eq!(
            TaskPeople::describe(ROLE_OWNER | ROLE_CREATOR),
            "负责人 / 创建人"
        );
    }

    // ---------- 进度：加权 / 等权 ----------

    #[test]
    fn weighted_progress_matches_the_card_example() {
        // 卡面验收项 7：3 个子任务，工时 1h / 2h / 7h，进度 0 / 50 / 100
        // 期望 (1×0 + 2×50 + 7×100) / 10 = 800 / 10 = 80
        let children = [
            child(TaskStatus::Doing, 0, Some(1.0)),
            child(TaskStatus::Doing, 50, Some(2.0)),
            child(TaskStatus::Doing, 100, Some(7.0)),
        ];
        assert_eq!(aggregate_progress(&children), Some(80));
        // 手算复核：(0 + 100 + 700) / 10 = 80.0
        assert_eq!((1.0 * 0.0 + 2.0 * 50.0 + 7.0 * 100.0) / 10.0, 80.0);
    }

    #[test]
    fn equal_weight_when_hours_are_missing() {
        // 卡面验收项 8：工时全空 → 等权平均 (0 + 50 + 100) / 3 = 50
        let children = [
            child(TaskStatus::Doing, 0, None),
            child(TaskStatus::Doing, 50, None),
            child(TaskStatus::Doing, 100, None),
        ];
        assert_eq!(aggregate_progress(&children), Some(50));
        // 工时「不全」也退化为等权（保守降级，避免空工时的子任务被当成 0 权重）
        let mixed = [
            child(TaskStatus::Doing, 0, Some(10.0)),
            child(TaskStatus::Doing, 100, None),
        ];
        assert_eq!(aggregate_progress(&mixed), Some(50));
        // 工时为 0 同样视为「没填」
        let zero = [
            child(TaskStatus::Doing, 0, Some(0.0)),
            child(TaskStatus::Doing, 100, Some(0.0)),
        ];
        assert_eq!(aggregate_progress(&zero), Some(50));
    }

    #[test]
    fn all_done_children_pin_parent_to_hundred_without_changing_status() {
        // 架构 D5：子任务全 DONE → 父进度 100，但**只算进度**，状态由调用方保持
        let children = [
            child(TaskStatus::Done, 100, Some(1.0)),
            child(TaskStatus::Done, 100, Some(2.0)),
        ];
        assert_eq!(aggregate_progress(&children), Some(100));
        assert!(ready_to_close(&children));
        // 有一条 CANCELLED 就不算「全 DONE」（进度照聚合算，不会被捷径拉满）
        let with_cancel = [
            child(TaskStatus::Done, 100, Some(1.0)),
            child(TaskStatus::Cancelled, 30, Some(1.0)),
        ];
        assert_eq!(aggregate_progress(&with_cancel), Some(65));
        assert!(!ready_to_close(&with_cancel));
    }

    #[test]
    fn aggregate_edges() {
        // 没有子任务 → None（父任务走手动进度）
        assert_eq!(aggregate_progress(&[]), None);
        assert!(!progress_locked(&[]));
        // 单子任务直接继承其进度
        assert_eq!(
            aggregate_progress(&[child(TaskStatus::Doing, 42, Some(3.0))]),
            Some(42)
        );
        // 四舍五入（不是截断）：两条 1h 子任务 33 / 34 → 33.5 → 34
        let rounding = [
            child(TaskStatus::Doing, 33, Some(1.0)),
            child(TaskStatus::Doing, 34, Some(1.0)),
        ];
        assert_eq!(aggregate_progress(&rounding), Some(34));
        // 有子任务即锁定手动进度
        assert!(progress_locked(&rounding));
    }

    #[test]
    fn open_children_counts_only_non_terminal() {
        let children = [
            child(TaskStatus::Todo, 0, Some(1.0)),
            child(TaskStatus::Doing, 30, Some(1.0)),
            child(TaskStatus::Review, 100, Some(1.0)),
            child(TaskStatus::Done, 100, Some(1.0)),
            child(TaskStatus::Cancelled, 10, Some(1.0)),
        ];
        assert_eq!(
            open_children(&children).len(),
            3,
            "TODO/DOING/REVIEW 才算未完成"
        );
        assert!(!ready_to_close(&children));
        assert!(open_children(&[]).is_empty());
    }

    #[test]
    fn out_edges_are_stable() {
        assert_eq!(outgoing(TaskStatus::Todo).len(), 2); // DOING / CANCELLED
        assert_eq!(outgoing(TaskStatus::Doing).len(), 3); // REVIEW / DONE / CANCELLED
        assert_eq!(outgoing(TaskStatus::Review).len(), 3); // DONE / DOING / CANCELLED
        assert_eq!(outgoing(TaskStatus::Done).len(), 1); // DOING（重开）
        assert_eq!(outgoing(TaskStatus::Cancelled).len(), 1); // TODO（恢复）
        assert_eq!(
            TaskStatus::ALL
                .iter()
                .map(|status| outgoing(*status).len())
                .sum::<usize>(),
            TRANSITIONS.len()
        );
    }

    #[test]
    fn status_parse_and_labels() {
        assert_eq!(TaskStatus::parse(" doing "), Some(TaskStatus::Doing));
        assert_eq!(TaskStatus::parse("cancelled"), Some(TaskStatus::Cancelled));
        assert_eq!(TaskStatus::parse("ARCHIVED"), None);
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::Review.is_terminal());
        assert_eq!(TaskStatus::Review.label(), "待验收");
        assert_eq!(clamp_progress(120), 100);
        assert_eq!(clamp_progress(-5), 0);
    }
}

// ==================== 单元测试：审批门闸（批次 D · D3a） ====================

#[cfg(test)]
mod gate_tests {
    use super::*;

    /// `Some` = 这张任务挂了审批单。
    fn bill() -> Option<Uuid> {
        Some(Uuid::from_u128(0xF10_0001))
    }

    // ---------- 清单 1/2：没挂审批单时，REVIEW 两条出边与 T2 完全一致 ----------

    #[test]
    fn without_approval_instance_the_gate_never_fires() {
        // 没挂单就不管：即便实例状态被误传成 PENDING 也不拦
        for to in [TaskStatus::Done, TaskStatus::Doing, TaskStatus::Cancelled] {
            assert!(!flow_gate_blocked(TaskStatus::Review, to, None, None));
            assert!(!flow_gate_blocked(
                TaskStatus::Review,
                to,
                None,
                Some("PENDING")
            ));
        }
        // 状态机口径不变：创建人 / 分派人 / 管理员可验收通过
        for mask in [ROLE_CREATOR, ROLE_ASSIGNER, ROLE_ADMIN] {
            assert!(
                check_transition(
                    ActorKind::User,
                    TaskStatus::Review,
                    TaskStatus::Done,
                    mask,
                    true,
                    None,
                    None,
                    None
                )
                .is_ok(),
                "无审批单时掩码 {mask} 应可验收通过"
            );
        }
        // 负责人仍**不是**验收人（T2 红线，不受本卡影响）
        assert_eq!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Done,
                ROLE_OWNER,
                true,
                None,
                None,
                None
            )
            .expect_err("负责人没有验收权"),
            TransitionCheckError::Transition(TransitionError::Forbidden {
                allowed: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
                actual: ROLE_OWNER,
            })
        );
        // 无审批单：REVIEW → DOING 打回照常（原因够 5 字即可）
        assert!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Doing,
                ROLE_CREATOR,
                true,
                Some("缺三处用例"),
                None,
                None
            )
            .is_ok()
        );
    }

    // ---------- 清单 3/4/5：PENDING 时，手工推 REVIEW 出边一律 409/65030 ----------

    #[test]
    fn pending_approval_blocks_review_exits_for_every_role() {
        // 创建人 / 分派人 / 管理员 手动 REVIEW → DONE
        for mask in [ROLE_CREATOR, ROLE_ASSIGNER, ROLE_ADMIN] {
            let error = check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Done,
                mask,
                true,
                None,
                bill(),
                Some("PENDING"),
            )
            .expect_err("审批中不许手工验收通过");
            assert_eq!(error, TransitionCheckError::ApprovalPending);
            assert!(error.is_gate());
            assert_eq!(error.message(), "任务正在审批中，请等待审批结果");
        }
        // 负责人 手动 REVIEW → DOING：门闸先于角色 → 也是 65030（不是 403）
        assert_eq!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Doing,
                ROLE_OWNER,
                true,
                Some("打回补用例"),
                bill(),
                Some("PENDING"),
            )
            .expect_err("审批中不许手工打回"),
            TransitionCheckError::ApprovalPending
        );
        // 协作人（掩码 0）同样拿到 65030：门闸不看你是谁
        assert_eq!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Done,
                0,
                true,
                None,
                bill(),
                Some("PENDING")
            )
            .expect_err("协作人也不许绕过审批"),
            TransitionCheckError::ApprovalPending
        );
        // 纯函数本身：两条 REVIEW 出边都被锁
        assert!(flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Done,
            bill(),
            Some("PENDING")
        ));
        assert!(flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Doing,
            bill(),
            Some("PENDING")
        ));
    }

    // ---------- 清单 6/7：审批结论落地后放行 ----------

    #[test]
    fn approved_releases_review_to_done_and_rejected_releases_reject() {
        // APPROVED → REVIEW → DONE 成功（流程已放行，验收人照常点）
        assert!(!flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Done,
            bill(),
            Some("APPROVED")
        ));
        let transition = check_transition(
            ActorKind::User,
            TaskStatus::Review,
            TaskStatus::Done,
            ROLE_CREATOR,
            true,
            None,
            bill(),
            Some("APPROVED"),
        )
        .expect("审批通过后必须能验收通过");
        assert_eq!(transition.to, TaskStatus::Done);
        assert_eq!(transition.progress, ProgressEffect::Force(100));
        // REJECTED → REVIEW → DOING 成功（驳回后人工返工）
        assert!(!flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Doing,
            bill(),
            Some("REJECTED")
        ));
        assert!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Doing,
                ROLE_ADMIN,
                true,
                Some("审批驳回后返工"),
                bill(),
                Some("REJECTED"),
            )
            .is_ok()
        );
    }

    // ---------- 清单 8：其它流转完全不受影响 ----------

    #[test]
    fn other_transitions_are_untouched_while_approval_pending() {
        // 只要不是 REVIEW 出边，门闸一概不响（含 REVIEW → CANCELLED：审批中也要能刹车）
        for (from, to) in [
            (TaskStatus::Todo, TaskStatus::Doing),
            (TaskStatus::Doing, TaskStatus::Review),
            (TaskStatus::Todo, TaskStatus::Cancelled),
            (TaskStatus::Doing, TaskStatus::Cancelled),
            (TaskStatus::Review, TaskStatus::Cancelled),
            (TaskStatus::Done, TaskStatus::Doing),
            (TaskStatus::Cancelled, TaskStatus::Todo),
        ] {
            assert!(
                !flow_gate_blocked(from, to, bill(), Some("PENDING")),
                "{} → {} 不该被审批门闸拦",
                from.code(),
                to.code()
            );
        }
        // 行为与 T2 逐条一致：这三种流转在审批中照样能推
        assert!(
            check_transition(
                ActorKind::User,
                TaskStatus::Todo,
                TaskStatus::Doing,
                ROLE_OWNER,
                true,
                None,
                bill(),
                Some("PENDING")
            )
            .is_ok()
        );
        assert!(
            check_transition(
                ActorKind::User,
                TaskStatus::Doing,
                TaskStatus::Review,
                ROLE_OWNER,
                true,
                None,
                bill(),
                Some("PENDING")
            )
            .is_ok()
        );
        for from in [TaskStatus::Todo, TaskStatus::Doing, TaskStatus::Review] {
            assert!(
                check_transition(
                    ActorKind::User,
                    from,
                    TaskStatus::Cancelled,
                    ROLE_ADMIN,
                    true,
                    None,
                    bill(),
                    Some("PENDING")
                )
                .is_ok(),
                "{} → CANCELLED 不该被拦",
                from.code()
            );
        }
        // 白名单本身没动：非法组合仍是 IllegalPair（400/409 系，不是 65030）
        assert_eq!(
            check_transition(
                ActorKind::User,
                TaskStatus::Todo,
                TaskStatus::Done,
                ROLE_ADMIN,
                false,
                None,
                bill(),
                Some("PENDING")
            )
            .expect_err("跳级必须仍被白名单拒绝"),
            TransitionCheckError::Transition(TransitionError::IllegalPair)
        );
        // 白名单条数也没变（红线：TRANSITIONS 一行没动）
        assert_eq!(TRANSITIONS.len(), 10);
    }

    // ---------- 边界：状态字面量归一化 / 读不到实例状态 ----------

    #[test]
    fn pending_match_is_case_insensitive_and_unknown_status_does_not_lock() {
        assert!(flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Done,
            bill(),
            Some(" pending ")
        ));
        assert!(flow_gate_blocked(
            TaskStatus::Review,
            TaskStatus::Done,
            bill(),
            Some("Pending")
        ));
        // 已结束 / 驳回 / 撤回 / 读不到 → 本门闸不拦（实例合法性归 flow 引擎 65024）
        for status in [
            Some("APPROVED"),
            Some("REJECTED"),
            Some("CANCELLED"),
            Some("WITHDRAWN"),
            None,
        ] {
            assert!(
                !flow_gate_blocked(TaskStatus::Review, TaskStatus::Done, bill(), status),
                "实例状态 {status:?} 不该被本门闸拦"
            );
        }
        // 起点不是 REVIEW 就与门闸无关
        assert!(!flow_gate_blocked(
            TaskStatus::Done,
            TaskStatus::Doing,
            bill(),
            Some("PENDING")
        ));
        assert!(!gate_applies(TaskStatus::Review, TaskStatus::Cancelled));
        assert!(gate_applies(TaskStatus::Review, TaskStatus::Done));
    }
}

// ==================== 单元测试：流程回调放行（批次 D · D3b） ====================

#[cfg(test)]
mod callback_tests {
    use super::*;

    /// `Some` = 这张任务挂了审批单（与门闸测试用不同的 id，避免「碰巧相等」掩盖 bug）。
    fn bill() -> Option<Uuid> {
        Some(Uuid::from_u128(0xF10_0002))
    }

    #[test]
    fn callback_bypasses_gate_on_review_to_done() {
        // 审批单还 PENDING、操作人角色掩码 0（不是任何人的身份）——回调照样能推进
        let transition = check_transition(
            ActorKind::FlowCallback,
            TaskStatus::Review,
            TaskStatus::Done,
            0,
            true,
            None,
            bill(),
            Some("PENDING"),
        )
        .expect("审批通过后流程回调必须能推 REVIEW → DONE");
        assert_eq!(transition.to, TaskStatus::Done);
        // 借的是表项的副作用：与人工验收同口径
        assert_eq!(transition.progress, ProgressEffect::Force(100));
        assert_eq!(transition.done_at, DoneAtEffect::SetNow);
        assert_eq!(transition.label, "验收通过");
    }

    #[test]
    fn callback_bypasses_role_and_reason_on_review_to_doing() {
        // 驳回意见不足 5 字也不拦（审批意见自带语义，不该被人工打回的字数规则挡住）
        let transition = check_transition(
            ActorKind::FlowCallback,
            TaskStatus::Review,
            TaskStatus::Doing,
            0,
            true,
            Some("不通过"),
            bill(),
            Some("PENDING"),
        )
        .expect("审批驳回后流程回调必须能推 REVIEW → DOING");
        assert_eq!(transition.to, TaskStatus::Doing);
        assert_eq!(transition.progress, ProgressEffect::Reset(10));
        assert_eq!(transition.done_at, DoneAtEffect::Keep);
        // 连原因都不给也能过（角色 / 附加条件全跳过）
        assert!(callback_transition(TaskStatus::Review, TaskStatus::Doing).is_ok());
    }

    #[test]
    fn callback_stays_inside_review() {
        // 只有 REVIEW 的两条出边对回调开放，其余一律 IllegalPair（哪怕白名单允许）
        for (from, to) in [
            (TaskStatus::Todo, TaskStatus::Doing),
            (TaskStatus::Doing, TaskStatus::Review),
            (TaskStatus::Doing, TaskStatus::Done),
            (TaskStatus::Done, TaskStatus::Doing),
            (TaskStatus::Cancelled, TaskStatus::Todo),
            (TaskStatus::Review, TaskStatus::Cancelled),
            (TaskStatus::Review, TaskStatus::Review),
        ] {
            assert_eq!(
                callback_transition(from, to).expect_err("回调只能在 REVIEW 态推两条出边"),
                TransitionError::IllegalPair,
                "{} → {} 不该对流程回调开放",
                from.code(),
                to.code()
            );
        }
    }

    #[test]
    fn callback_identity_does_not_leak_into_user_path() {
        // 同一组输入换成人类身份：门闸照旧（65030），说明放行只属于回调分支
        assert_eq!(
            check_transition(
                ActorKind::User,
                TaskStatus::Review,
                TaskStatus::Done,
                0,
                true,
                None,
                bill(),
                Some("PENDING"),
            )
            .expect_err("人类身份在审批中仍被拦"),
            TransitionCheckError::ApprovalPending
        );
        assert!(!ActorKind::User.is_callback());
        assert!(ActorKind::FlowCallback.is_callback());
        // 白名单依旧 10 条（红线：TRANSITIONS 一行没动）
        assert_eq!(TRANSITIONS.len(), 10);
    }
}
