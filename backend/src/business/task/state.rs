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
