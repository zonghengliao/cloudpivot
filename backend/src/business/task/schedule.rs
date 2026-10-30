//! 甘特拖拽改期（批次 C+ · **T7a**）：接口 + 四类校验（纯逻辑可无库单测）。
//!
//! 依据：`docs/云枢设计修正补丁-20261029.md` **修正 10**（拖拽改期必须重跑依赖校验）、
//! `docs/批次C+-工作任务管理-核心设计.md` §四（FS 依赖）/ §五（甘特图）、
//! 卡面 t_52ce6d07 的「一、后端 / 二、前端」两段。
//!
//! # 本模块存在的理由
//!
//! T4 交的甘特图是**只读**的：任务条一旦能拖，「日期」就从「表单里的一个字段」
//! 变成「一条随时被改的排期约束」。如果只写 `UPDATE biz_task SET start_date = ?`，
//! 会出现三种立刻可见的坏数据：
//!
//! 1. 后继任务被拖到前置**没做完之前**（「前置都没完成，我怎么就开始了」）；
//! 2. 前置任务被拖到后继**已经计划开始之后**（后继的排期变成谎言）；
//! 3. 子任务被拖出父任务的区间（父任务进度按子任务汇总，区间不一致会让甘特图自相矛盾）。
//!
//! 所以本模块的顺序是「**解析目标 → 日期合法性 → 前置 → 后继 → 父子 → 落库**」，
//! 四类校验**全部在同一个事务里、基于事务内刚读到的快照**跑 —— 这不是形式主义：
//! 拖拽是高频写操作，两次改期并发时，任何「事务外读的依赖集合」都可能已经过期。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏路径**不写** `/api`）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | PATCH | `/business/task/{id}/schedule` | `business:task:update` | 改期（平移 / 显式日期）+ 重跑依赖校验 |
//!
//! # 五条硬约束（逐条对应卡面红线）
//!
//! 1. **改期后必须重跑依赖校验**（修正 10 的核心）：四类校验在事务内跑，
//!    与「加依赖时的环检测」同一事务串行化口径。
//! 2. **错误信息必须点名冲突对象**：`task_no + 标题 + 冲突日期` 一样不能少
//!    （[`check_predecessors`] / [`check_successors`] / [`check_parent_range`] /
//!    [`check_children_range`] 的文案是唯一实现，前端直接展示后端 `msg`）。
//! 3. **一次报全**：同一类冲突有多条时逐条列出（`、` 分隔），不要「修一个冒一个」。
//! 4. **不级联**（卡面「本卡默认不级联，只改自己」）：父任务改期**不改**子任务，
//!    因此父任务如果把子任务甩出区间，直接 `400 + 63112` 拒绝 —— 而不是悄悄把子任务也挪走。
//! 5. **留痕两件套**：`SCHEDULE` 类型评论（时间线上人看得见）+ `sys_change_log`
//!    （`action = UPDATE`，diff 只含 `start_date` / `due_date`）。
//!
//! # 权限（为什么中间件闸是 `business:task:update` 而不是新码）
//!
//! 卡面明确「复用，不新增」。行级判定与编辑接口同口径（负责人 / 创建人），
//! 额外放行超管（与删除接口一致：管理员要能救场）。
//!
//! SCOPE-EXEMPT: 本文件不做数据范围过滤 —— 可见性判定走
//! [`model::find_detail`]（`scope ∪ 参与关系` 谓词**唯一实现**），
//! 之后的依赖 / 父子查询都以 `task_id` 收口。再注入一次 scope 会把
//! 「看不见的前置」过滤掉，导致改期校验**静默漏判**（比越权更难发现）。

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::{Duration, NaiveDate};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput, RecordOutcome};
use crate::business::shared::principal;
use crate::business::task::dep;
use crate::business::task::dto::{ScheduleMode, TaskScheduleReq, TaskScheduleRes};
use crate::business::task::handler::{
    current_user, display_name, not_found, param_invalid, parse_uuid_field,
};
use crate::business::task::model::{self, PeerSchedule, ScheduleRow};
use crate::business::task::state::TaskStatus;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 事务闭包里的错误类型只能是 [`DbError`]，所以「业务冲突（码值 + 文案）」要编码进
/// `DbError::InvalidValue` 的字符串再在事务外还原。
///
/// 形态：`TASK_SCHEDULE_CONFLICT:{code}|{message}`。
/// 与 T4 的 `TASK_DEP_CYCLE:` 前缀同一手法（`add_dependency` 的先例），
/// 前缀存在的意义是「业务拒绝」与「真正的数据库异常（→ 500）」分得开。
const SCHEDULE_CONFLICT_PREFIX: &str = "TASK_SCHEDULE_CONFLICT:";

/// 改期咨询锁的 key（`pg_advisory_xact_lock`）。
///
/// 为什么需要：`shift_days` 的语义是「在当前日期上平移 N 天」，
/// 两次并发 +3 天的正确结果只能是 +6（或后一次基于新值 +3），
/// 绝不能两次都读到旧值各写 +3（结果仍 +3，用户会以为丢了一次拖动）。
/// 改期的写入频率远低于读取，串行化没有代价。
const SCHEDULE_ADVISORY_LOCK: i64 = 7_340_107;

/// 「不得早于 `created_at` 前 N 天」的下界（卡面：前 1 年，防误输）。
const CREATED_AT_FLOOR_DAYS: i64 = 365;

// ==================== 纯逻辑：目标日期 ====================

/// 一条 FS 依赖的**方向**（与 [`dep::DepEdge`] 同口径，这里只用名字）：
/// `depends_on_id` 是前置（先做完），`task_id` 是后继（被阻塞）。
///
/// 本模块只需要「前置组」与「后继组」两个方向，故不重复定义边类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetSpan {
    /// 改期后的计划开始日。
    pub start: Option<NaiveDate>,
    /// 改期后的截止日。
    pub due: Option<NaiveDate>,
}

impl TargetSpan {
    /// 构造。
    pub fn new(start: Option<NaiveDate>, due: Option<NaiveDate>) -> Self {
        Self { start, due }
    }

    /// 参与**前置**判定的「开始日」：有 `start` 用它；只有 `due` 时用 `due`
    /// （一个「只排到某天结束」的任务，那天之前都没空开工，等价于开始日）。
    pub fn effective_start(&self) -> Option<NaiveDate> {
        self.start.or(self.due)
    }

    /// 参与**后继**判定的「结束日」：有 `due` 用它；只有 `start` 时用 `start`。
    pub fn effective_due(&self) -> Option<NaiveDate> {
        self.due.or(self.start)
    }

    /// `2026-11-01 ~ 2026-11-05` / `—` 形态（文案与前端同款）。
    pub fn text(&self) -> String {
        render_span(self.start, self.due)
    }
}

/// 把区间渲染成 `2026-11-01 ~ 2026-11-05`（空值用 `—`）。
pub fn render_span(start: Option<NaiveDate>, due: Option<NaiveDate>) -> String {
    let start_text = start
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "—".to_string());
    let due_text = due
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "—".to_string());
    format!("{start_text} ~ {due_text}")
}

/// 一次改期冲突：**码值 + 已渲染的完整文案**。
///
/// 文案在这里拼好（不在 handler 里拼）的原因：纯函数单测可以直接断言
/// 「有没有点名 task_no / 标题 / 日期」，而不用起一个 HTTP 服务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleConflict {
    /// 业务错误码（`63110` / `63111` / `63112` / `63004`）。
    pub code: i32,
    /// 面向用户的完整文案（**已包含 task_no + 标题 + 日期**）。
    pub message: String,
}

impl ScheduleConflict {
    /// 构造。
    pub fn new(code: i32, message: String) -> Self {
        Self { code, message }
    }

    /// 编码成可塞进 `DbError::InvalidValue` 的字符串。
    pub fn encode(&self) -> String {
        format!("{SCHEDULE_CONFLICT_PREFIX}{}|{}", self.code, self.message)
    }

    /// 从 `DbError::InvalidValue` 的字符串还原（前缀不匹配 → `None`）。
    pub fn decode(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix(SCHEDULE_CONFLICT_PREFIX)?;
        let (code, message) = rest.split_once('|')?;
        Some(Self {
            code: code.parse().ok()?,
            message: message.to_string(),
        })
    }
}

/// 解析本次改期的目标区间。
///
/// # 参数
///
/// - `current`：**事务内**读到的当前排期快照（平移的基准值）；
/// - `req`：原始请求；
/// - `mode`：已校验过的形态（[`TaskScheduleReq::mode`]）。
///
/// # 返回值
///
/// 目标区间（两个日期都可能为空 —— 例如一条只有截止日的任务被左边缘拖动前的形态）。
///
/// # 异常情况
///
/// 返回 `Err(中文消息)`（handler 映射 `400 + 63004`）：
/// - 平移形态下任务**两个日期都没有**（没有基准可平移）；
/// - 显式形态下改完仍然两个日期都没有。
pub fn resolve_target(
    current: &ScheduleRow,
    req: &TaskScheduleReq,
    mode: ScheduleMode,
) -> std::result::Result<TargetSpan, String> {
    let span = match mode {
        ScheduleMode::Shift(days) => {
            if current.start_date.is_none() && current.due_date.is_none() {
                return Err(
                    "任务没有起止日期，无法整体平移（请先在详情页设置起止日期）".to_string()
                );
            }
            let delta = Duration::days(days);
            TargetSpan::new(
                current.start_date.map(|date| date + delta),
                current.due_date.map(|date| date + delta),
            )
        }
        ScheduleMode::Explicit => TargetSpan::new(
            req.start_date.or(current.start_date),
            req.due_date.or(current.due_date),
        ),
    };
    if span.start.is_none() && span.due.is_none() {
        return Err("改期后任务没有任何日期（起止日不能同时为空）".to_string());
    }
    Ok(span)
}

// ==================== 纯逻辑：四类校验 ====================

/// ① 日期合法性：`start <= due`，且都不早于 `created_at - 1 年`。
///
/// # 为什么下界用「创建时间前 1 年」而不是「今天前 N 年」
///
/// 补录历史任务是合法需求（把去年的活儿补录进系统），所以不能卡「今天」；
/// 但 `1000-01-01` 这种明显是误输（拖拽像素换算错 / 手输少一位），
/// 以任务创建时间为锚、放宽 1 年，既不误伤补录也能拦住手滑。
pub fn check_date_sanity(
    span: &TargetSpan,
    created_at: NaiveDate,
) -> std::result::Result<(), ScheduleConflict> {
    if let (Some(start), Some(due)) = (span.start, span.due)
        && start > due
    {
        return Err(ScheduleConflict::new(
            error_code::TASK_FIELD_INVALID,
            format!(
                "日期非法：计划开始日 {} 不能晚于截止日 {}（本次改期目标 {}）",
                start.format("%Y-%m-%d"),
                due.format("%Y-%m-%d"),
                span.text()
            ),
        ));
    }
    let floor = created_at - Duration::days(CREATED_AT_FLOOR_DAYS);
    for (label, value) in [("计划开始日", span.start), ("截止日", span.due)] {
        let Some(value) = value else { continue };
        if value < floor {
            return Err(ScheduleConflict::new(
                error_code::TASK_FIELD_INVALID,
                format!(
                    "日期非法：{label} {} 早于允许的下界 {}（任务创建于 {}，不得早于创建时间前 1 年）",
                    value.format("%Y-%m-%d"),
                    floor.format("%Y-%m-%d"),
                    created_at.format("%Y-%m-%d")
                ),
            ));
        }
    }
    Ok(())
}

/// ② 前置约束：改期后本任务「开始」不得早于任何**未完成**前置的截止日。
///
/// # 口径
///
/// - 前置「已解除」的判据与进 DOING 的 409 校验完全相同
///   （[`dep::is_blocking_status`]：`DONE` / `CANCELLED` 解除，其余阻塞）；
/// - 前置**没有截止日** ⇒ 无法比较，不拦（但会在消息里不出现，属有意：拿不准的
///   不制造假冲突）；
/// - 相等**允许**（「不得早于」= `start >= pred.due`）。
pub fn check_predecessors(
    span: &TargetSpan,
    prerequisites: &[PeerSchedule],
) -> std::result::Result<(), ScheduleConflict> {
    let Some(start) = span.effective_start() else {
        return Ok(());
    };
    let hits: Vec<String> = prerequisites
        .iter()
        .filter(|peer| dep::is_blocking_status(&peer.status))
        .filter_map(|peer| {
            let due = peer.due_date?;
            (start < due).then(|| {
                format!(
                    "前置 {}「{}」（截止 {}）尚未完成",
                    peer.task_no,
                    peer.title,
                    due.format("%Y-%m-%d")
                )
            })
        })
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    Err(ScheduleConflict::new(
        error_code::TASK_SCHEDULE_PREDECESSOR,
        format!(
            "改期违反依赖：{}（本次改期后开始于 {}）",
            hits.join("、"),
            start.format("%Y-%m-%d")
        ),
    ))
}

/// ③ 后继约束：改期后本任务「截止」不得晚于任何后继的计划开始日。
///
/// # 口径
///
/// - `CANCELLED` 的后继已无排期意义，不参与（其余状态一律参与，含 `DONE`：
///   一条已完成的后继仍记录着「它当时从哪天开始」，前置拖到它之后会让历史自相矛盾）；
/// - 后继**没有开始日** ⇒ 不拦；
/// - 相等**允许**（`due <= succ.start`）。
pub fn check_successors(
    span: &TargetSpan,
    successors: &[PeerSchedule],
) -> std::result::Result<(), ScheduleConflict> {
    let Some(due) = span.effective_due() else {
        return Ok(());
    };
    let hits: Vec<String> = successors
        .iter()
        .filter(|peer| {
            !peer
                .status
                .eq_ignore_ascii_case(TaskStatus::Cancelled.code())
        })
        .filter_map(|peer| {
            let start = peer.start_date?;
            (due > start).then(|| {
                format!(
                    "后继 {}「{}」计划 {} 开始",
                    peer.task_no,
                    peer.title,
                    start.format("%Y-%m-%d")
                )
            })
        })
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    Err(ScheduleConflict::new(
        error_code::TASK_SCHEDULE_SUCCESSOR,
        format!(
            "改期违反依赖：{}（本次改期后截止于 {}）",
            hits.join("、"),
            due.format("%Y-%m-%d")
        ),
    ))
}

/// ④-a 父子约束（本任务当**子**）：新日期必须落在父任务区间内（父有排期时）。
pub fn check_parent_range(
    span: &TargetSpan,
    parent: Option<&PeerSchedule>,
) -> std::result::Result<(), ScheduleConflict> {
    let Some(parent) = parent else {
        return Ok(());
    };
    if parent.start_date.is_none() && parent.due_date.is_none() {
        return Ok(()); // 父任务没排期 ⇒ 无区间可违反
    }
    let mut violations: Vec<String> = Vec::new();
    if let (Some(child_start), Some(parent_start)) = (span.start, parent.start_date)
        && child_start < parent_start
    {
        violations.push(format!(
            "开始日 {} 早于父任务开始日 {}",
            child_start.format("%Y-%m-%d"),
            parent_start.format("%Y-%m-%d")
        ));
    }
    if let (Some(child_due), Some(parent_due)) = (span.due, parent.due_date)
        && child_due > parent_due
    {
        violations.push(format!(
            "截止日 {} 晚于父任务截止日 {}",
            child_due.format("%Y-%m-%d"),
            parent_due.format("%Y-%m-%d")
        ));
    }
    if violations.is_empty() {
        return Ok(());
    }
    Err(ScheduleConflict::new(
        error_code::TASK_SCHEDULE_PARENT_RANGE,
        format!(
            "子任务日期必须在父任务范围内：父任务 {}「{}」（{}），本次改期 {}：{}",
            parent.task_no,
            parent.title,
            render_span(parent.start_date, parent.due_date),
            span.text(),
            violations.join("；")
        ),
    ))
}

/// ④-b 父子约束（本任务当**父**）：**不级联**，所以父任务改期不许把已有子任务甩出区间。
///
/// # 为什么这条也在 63112 里
///
/// 前端文案只有一句「子任务日期必须在父任务范围内」。把「父任务甩出子任务」拆成
/// 另一个码，前端就得再写一套文案，而用户看到的问题本质是同一个
/// （父子区间不一致）。文案里点名**具体的子任务**，两种方向都能一眼定位。
pub fn check_children_range(
    span: &TargetSpan,
    children: &[PeerSchedule],
) -> std::result::Result<(), ScheduleConflict> {
    if span.start.is_none() && span.due.is_none() {
        return Ok(());
    }
    let mut hits: Vec<String> = Vec::new();
    for child in children {
        if child.start_date.is_none() && child.due_date.is_none() {
            continue; // 未排期的子任务不受区间约束
        }
        let outside = match (child.start_date, span.start) {
            (Some(child_start), Some(parent_start)) => child_start < parent_start,
            _ => false,
        } || match (child.due_date, span.due) {
            (Some(child_due), Some(parent_due)) => child_due > parent_due,
            _ => false,
        };
        if outside {
            hits.push(format!(
                "子任务 {}「{}」（{}）",
                child.task_no,
                child.title,
                render_span(child.start_date, child.due_date)
            ));
        }
    }
    if hits.is_empty() {
        return Ok(());
    }
    Err(ScheduleConflict::new(
        error_code::TASK_SCHEDULE_PARENT_RANGE,
        format!(
            "子任务日期必须在父任务范围内：本次改期后父任务为 {}（本版不级联改子任务），\
             会被甩出区间的有 {}，请先调整子任务日期",
            span.text(),
            hits.join("、")
        ),
    ))
}

/// 四类校验按卡面顺序跑一遍（**任一失败立即返回**，错误只报第一类）。
///
/// # 为什么「只报第一类」
///
/// 一次拖动同时违反「日期非法」和「前置未完成」时，先修日期才有意义
/// （日期本身不合法时，依赖判定的输入是废话）。同类内部则一次报全
/// （见各 `check_*` 的 `hits.join`），避免「修一个冒一个」。
pub fn check_all(
    span: &TargetSpan,
    current: &ScheduleRow,
    prerequisites: &[PeerSchedule],
    successors: &[PeerSchedule],
    parent: Option<&PeerSchedule>,
    children: &[PeerSchedule],
) -> std::result::Result<(), ScheduleConflict> {
    check_date_sanity(span, model::cst_date_of(current.created_at))?;
    check_predecessors(span, prerequisites)?;
    check_successors(span, successors)?;
    check_parent_range(span, parent)?;
    check_children_range(span, children)?;
    Ok(())
}

// ==================== 接口 ====================

/// 改期（`PATCH /business/task/{id}/schedule`）。
///
/// # 请求
///
/// `{start_date?, due_date?, shift_days?}`，形态见 [`TaskScheduleReq`]：
/// 平移（`shift_days`）与显式日期**二选一**。
///
/// # 校验顺序（任一失败 → 400 + 点名原因）
///
/// 1. 形态：`shift_days` 与显式日期二选一、`shift_days != 0`、绝对值 ≤ 3650（`63004`）；
/// 2. 日期合法性：`start <= due`、不早于 `created_at - 1 年`（`63004`）；
/// 3. 前置：改期后开始日不得早于未完成前置的截止日（`63110`，点名前置）；
/// 4. 后继：改期后截止日不得晚于后继的计划开始日（`63111`，点名后继）；
/// 5. 父子：子任务必须落在父任务区间内 + 父任务不许把子任务甩出区间（`63112`，点名父子）。
///
/// # 副作用（同一事务）
///
/// - 写 `start_date` / `due_date`；
/// - 写一条 `comment_type = SCHEDULE` 的评论；
/// - 写 `sys_change_log`（`action = UPDATE`，diff 只含这两列）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 403 + `63002`：不是负责人 / 创建人 / 超管；
/// - 400 + `63004`：请求形态或日期非法（或改期后与原值相同）；
/// - 400 + `63110` / `63111` / `63112`：依赖 / 父子冲突（**点名冲突对象**）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/schedule",
    method = "PATCH",
    permission = "business:task:update"
)]
#[nik::patch("/business/task/{id}/schedule")]
pub async fn schedule(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskScheduleReq>,
) -> ResResult<TaskScheduleRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let mode = req.mode().map_err(param_invalid)?;

    // ① 可见性（唯一实现：scope ∪ 参与关系）—— 看不见就一定 404，不暴露存在性
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // ② 写权限：负责人 / 创建人 / 超管（与编辑、删除同口径）
    if current.card.owner_id != me && current.creator_id != me && !mine.is_super() {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的负责人、创建人或管理员可以改期",
        ));
    }

    let operator_name = display_name(&claims);
    let operator_name_tx = operator_name.clone();

    // ③ 事务：串行化 → 重读快照 → 四类校验 → 落库 → 评论 → 变更日志
    let outcome = db()
        .transaction(|tx| async move {
            // 事务级咨询锁：**必须用 exec_sql**（`SELECT pg_advisory_xact_lock()` 返回
            // void 列，走 query_sql 会在驱动层 deserialize 失败 —— T4 实测 500/10004）
            tx.exec_sql(
                "SELECT pg_advisory_xact_lock(?::int8)".to_string(),
                vec![Value::I64(SCHEDULE_ADVISORY_LOCK)],
            )
            .await?;

            let Some(row) = model::find_schedule_row(&tx, task_id).await? else {
                return Ok(None); // 事务内被并发软删 → 调用方转 404
            };
            let span = resolve_target(&row, &req, mode).map_err(|message| {
                DbError::InvalidValue(
                    ScheduleConflict::new(error_code::TASK_FIELD_INVALID, message).encode(),
                )
            })?;
            if span.start == row.start_date && span.due == row.due_date {
                return Err(DbError::InvalidValue(
                    ScheduleConflict::new(
                        error_code::TASK_FIELD_INVALID,
                        format!(
                            "改期没有实际变化：仍为 {}（拖动 0 格时前端不应发请求）",
                            span.text()
                        ),
                    )
                    .encode(),
                ));
            }

            let prerequisites = model::schedule_prerequisites(&tx, task_id).await?;
            let successors = model::schedule_successors(&tx, task_id).await?;
            let parent = match row.parent_id {
                Some(parent_id) => model::schedule_parent(&tx, parent_id).await?,
                None => None,
            };
            let children = model::schedule_children(&tx, task_id).await?;
            check_all(
                &span,
                &row,
                &prerequisites,
                &successors,
                parent.as_ref(),
                &children,
            )
            .map_err(|conflict| DbError::InvalidValue(conflict.encode()))?;

            let affected = model::update_schedule(&tx, task_id, span.start, span.due, me).await?;
            if affected == 0 {
                return Ok(None); // 并发软删：评论与日志都不该落
            }
            let comment = format!(
                "改期：{} → {}（{}）",
                render_span(row.start_date, row.due_date),
                span.text(),
                match mode {
                    ScheduleMode::Shift(days) => format!("整体平移 {}", signed_days(days)),
                    ScheduleMode::Explicit => "手动指定日期".to_string(),
                }
            );
            let comment_id =
                model::insert_comment(&tx, task_id, me, &comment, model::COMMENT_TYPE_SCHEDULE, me)
                    .await?;
            let before = serde_json::json!({
                "start_date": row.start_date,
                "due_date": row.due_date,
            });
            let after = serde_json::json!({
                "start_date": span.start,
                "due_date": span.due,
            });
            let recorded = changelog::record(
                &tx,
                &ChangeCtx::api(Some(me), Some(&operator_name_tx)),
                ChangeTarget::of(
                    "biz_task",
                    task_id.to_string(),
                    Action::Update,
                    "甘特拖拽改期",
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
            Ok(Some(ScheduleOutcome {
                task_no: row.task_no,
                old_start: row.start_date,
                old_due: row.due_date,
                comment_id,
                changelog_written: matches!(recorded, RecordOutcome::Recorded { .. }),
            }))
        })
        .await;

    let outcome = match outcome {
        Ok(Some(value)) => value,
        Ok(None) => return Err(not_found()),
        Err(DbError::InvalidValue(message)) => {
            if let Some(conflict) = ScheduleConflict::decode(&message) {
                return Err(ApiError::new(400, conflict.code, conflict.message));
            }
            return Err(utils::db_error(DbError::InvalidValue(message), "改期"));
        }
        Err(error) => return Err(utils::db_error(error, "改期")),
    };

    // ④ 事务外回读（贴库值：前端乐观更新要按库里的日期纠正自己的预演值）
    let fresh = model::find_schedule_row(db(), task_id)
        .await
        .map_err(|error| utils::db_error(error, "回读任务排期"))?
        .ok_or_else(not_found)?;
    let shift_days = shift_between(outcome.old_start, fresh.start_date)
        .or_else(|| shift_between(outcome.old_due, fresh.due_date))
        .unwrap_or(0);

    tracing::info!(
        task_id = %task_id,
        task_no = %outcome.task_no,
        old_start = ?outcome.old_start,
        old_due = ?outcome.old_due,
        new_start = ?fresh.start_date,
        new_due = ?fresh.due_date,
        operator = %me,
        "任务已改期（依赖校验通过）"
    );
    Ok(Res::ok(TaskScheduleRes {
        id: task_id.to_string(),
        task_no: outcome.task_no,
        old_start_date: outcome.old_start,
        old_due_date: outcome.old_due,
        start_date: fresh.start_date,
        due_date: fresh.due_date,
        shift_days,
        comment_id: outcome.comment_id.to_string(),
        changelog_written: outcome.changelog_written,
    }))
}

/// 改期事务的产出（事务闭包里组装，事务外拼出参）。
struct ScheduleOutcome {
    /// 任务编号。
    task_no: String,
    /// 改期前开始日。
    old_start: Option<NaiveDate>,
    /// 改期前截止日。
    old_due: Option<NaiveDate>,
    /// `SCHEDULE` 评论 id。
    comment_id: Uuid,
    /// 变更日志是否真的落了一行。
    changelog_written: bool,
}

/// `+3 天` / `-2 天`（评论正文用；`0` 只会出现在「无变化」被拒之前）。
fn signed_days(days: i64) -> String {
    if days >= 0 {
        format!("+{days} 天")
    } else {
        format!("{days} 天")
    }
}

/// 两个日期相差几天（任一为空 → `None`）。
fn shift_between(from: Option<NaiveDate>, to: Option<NaiveDate>) -> Option<i64> {
    Some((to? - from?).num_days())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::task::dto::MAX_SHIFT_DAYS;
    use nik::prelude::chrono::{TimeZone, Utc};

    /// 测试固定的「今天」：2026-10-29（与本项目所有验收脚本同一口径）。
    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("测试日期必须是 YYYY-MM-DD")
    }

    fn row(start: Option<&str>, due: Option<&str>) -> ScheduleRow {
        ScheduleRow {
            id: Uuid::now_v7(),
            task_no: "TSK-202610-000007".to_string(),
            title: "被改期的任务".to_string(),
            status: "TODO".to_string(),
            start_date: start.map(day),
            due_date: due.map(day),
            created_at: Utc
                .with_ymd_and_hms(2026, 10, 1, 3, 0, 0)
                .single()
                .expect("合法时间"),
            parent_id: None,
        }
    }

    fn peer(
        task_no: &str,
        title: &str,
        status: &str,
        start: Option<&str>,
        due: Option<&str>,
    ) -> PeerSchedule {
        PeerSchedule {
            peer_id: Uuid::now_v7(),
            task_no: task_no.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            start_date: start.map(day),
            due_date: due.map(day),
        }
    }

    // ---------- 形态 ----------

    #[test]
    fn mode_rejects_both_or_neither_or_zero() {
        let both = TaskScheduleReq {
            start_date: Some(day("2026-11-01")),
            due_date: None,
            shift_days: Some(3),
        };
        assert!(both.mode().is_err(), "两种形态同时出现必须 400（不许猜）");

        let neither = TaskScheduleReq::default();
        assert!(neither.mode().is_err(), "空请求必须 400");

        let zero = TaskScheduleReq {
            shift_days: Some(0),
            ..TaskScheduleReq::default()
        };
        assert!(zero.mode().is_err(), "平移 0 天等于没改期");

        let huge = TaskScheduleReq {
            shift_days: Some(MAX_SHIFT_DAYS + 1),
            ..TaskScheduleReq::default()
        };
        assert!(huge.mode().is_err(), "超过上限的平移必须拒绝");

        assert_eq!(
            TaskScheduleReq {
                shift_days: Some(-3),
                ..TaskScheduleReq::default()
            }
            .mode(),
            Ok(ScheduleMode::Shift(-3))
        );
        assert_eq!(
            TaskScheduleReq {
                due_date: Some(day("2026-11-01")),
                ..TaskScheduleReq::default()
            }
            .mode(),
            Ok(ScheduleMode::Explicit)
        );
    }

    #[test]
    fn shift_moves_both_dates_by_same_delta() {
        let current = row(Some("2026-11-01"), Some("2026-11-05"));
        let req = TaskScheduleReq {
            shift_days: Some(3),
            ..TaskScheduleReq::default()
        };
        let span = resolve_target(&current, &req, ScheduleMode::Shift(3)).expect("合法平移");
        assert_eq!(span.start, Some(day("2026-11-04")));
        assert_eq!(span.due, Some(day("2026-11-08")));

        let back = resolve_target(&current, &req, ScheduleMode::Shift(-2)).expect("负平移");
        assert_eq!(back.start, Some(day("2026-10-30")));
        assert_eq!(back.due, Some(day("2026-11-03")));
    }

    #[test]
    fn explicit_keeps_unmentioned_date() {
        let current = row(Some("2026-11-01"), Some("2026-11-05"));
        // 左边缘拖：只给 start_date
        let req = TaskScheduleReq {
            start_date: Some(day("2026-11-03")),
            ..TaskScheduleReq::default()
        };
        let span = resolve_target(&current, &req, ScheduleMode::Explicit).expect("只改开始日");
        assert_eq!(span.start, Some(day("2026-11-03")));
        assert_eq!(
            span.due,
            Some(day("2026-11-05")),
            "未传的截止日必须保持原值"
        );

        // 右边缘拖：只给 due_date
        let req = TaskScheduleReq {
            due_date: Some(day("2026-11-09")),
            ..TaskScheduleReq::default()
        };
        let span = resolve_target(&current, &req, ScheduleMode::Explicit).expect("只改截止日");
        assert_eq!(
            span.start,
            Some(day("2026-11-01")),
            "未传的开始日必须保持原值"
        );
        assert_eq!(span.due, Some(day("2026-11-09")));
    }

    #[test]
    fn shift_without_any_date_is_rejected() {
        let current = row(None, None);
        let req = TaskScheduleReq {
            shift_days: Some(1),
            ..TaskScheduleReq::default()
        };
        assert!(
            resolve_target(&current, &req, ScheduleMode::Shift(1)).is_err(),
            "没有任何日期的任务无法「平移」，必须 400 而不是静默成功"
        );
    }

    // ---------- ① 日期合法性 ----------

    #[test]
    fn date_sanity_rejects_reversed_and_too_old() {
        let created = day("2026-10-01");
        let reversed = TargetSpan::new(Some(day("2026-11-10")), Some(day("2026-11-05")));
        let conflict = check_date_sanity(&reversed, created).expect_err("开始晚于截止必须拒");
        assert_eq!(conflict.code, error_code::TASK_FIELD_INVALID);
        assert!(
            conflict.message.contains("2026-11-10") && conflict.message.contains("2026-11-05"),
            "文案必须点名两个日期：{}",
            conflict.message
        );

        // 下界 = created_at - 365 天 = 2025-10-01
        let too_old = TargetSpan::new(Some(day("2025-09-30")), Some(day("2026-11-05")));
        let conflict = check_date_sanity(&too_old, created).expect_err("早于下界必须拒");
        assert!(
            conflict.message.contains("2025-10-01"),
            "文案必须点名下界：{}",
            conflict.message
        );

        // 边界：正好等于下界允许
        assert!(
            check_date_sanity(
                &TargetSpan::new(Some(day("2025-10-01")), Some(day("2026-11-05"))),
                created
            )
            .is_ok(),
            "正好落在下界上允许（不得早于 ≠ 必须晚于）"
        );
        // 只改截止日也要查下界
        assert!(
            check_date_sanity(&TargetSpan::new(None, Some(day("2020-01-01"))), created).is_err()
        );
    }

    // ---------- ② 前置 ----------

    #[test]
    fn predecessor_check_names_task_no_title_and_due() {
        let preds = vec![peer(
            "TSK-202610-000012",
            "数据库设计",
            "DOING",
            Some("2026-10-20"),
            Some("2026-10-30"),
        )];
        // 改到 10-29 开始（早于前置截止 10-30）→ 冲突
        let span = TargetSpan::new(Some(day("2026-10-29")), Some(day("2026-11-02")));
        let conflict = check_predecessors(&span, &preds).expect_err("早于前置截止必须拒");
        assert_eq!(conflict.code, error_code::TASK_SCHEDULE_PREDECESSOR);
        assert_eq!(conflict.code, 63110);
        assert!(
            conflict.message.contains("TSK-202610-000012"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.contains("数据库设计"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.contains("2026-10-30"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.starts_with("改期违反依赖：前置 "),
            "文案前缀必须是卡面写死的形态：{}",
            conflict.message
        );

        // 正好等于前置截止日 → 允许（不得早于）
        assert!(
            check_predecessors(
                &TargetSpan::new(Some(day("2026-10-30")), Some(day("2026-11-02"))),
                &preds
            )
            .is_ok()
        );
    }

    #[test]
    fn completed_or_cancelled_predecessor_does_not_block() {
        let span = TargetSpan::new(Some(day("2026-10-01")), Some(day("2026-10-05")));
        for status in ["DONE", "CANCELLED"] {
            let preds = vec![peer(
                "TSK-202610-000012",
                "数据库设计",
                status,
                Some("2026-09-01"),
                Some("2026-09-30"),
            )];
            assert!(
                check_predecessors(&span, &preds).is_ok(),
                "{status} 的前置已解除，不该拦改期"
            );
        }
        // REVIEW 仍算阻塞（与 409/63102 同口径）
        let preds = vec![peer(
            "TSK-202610-000013",
            "待验收的前置",
            "REVIEW",
            Some("2026-09-01"),
            Some("2026-10-10"),
        )];
        assert!(
            check_predecessors(&span, &preds).is_err(),
            "REVIEW 不是「已完成」"
        );
    }

    #[test]
    fn predecessor_without_due_date_is_not_a_conflict() {
        let preds = vec![peer(
            "TSK-202610-000014",
            "没排截止日的前置",
            "TODO",
            None,
            None,
        )];
        let span = TargetSpan::new(Some(day("2026-10-01")), Some(day("2026-10-05")));
        assert!(
            check_predecessors(&span, &preds).is_ok(),
            "前置没有截止日 = 无法判定，不制造假冲突"
        );
    }

    // ---------- ③ 后继 ----------

    #[test]
    fn successor_check_names_task_no_title_and_start() {
        let succs = vec![peer(
            "TSK-202610-000015",
            "接口联调",
            "TODO",
            Some("2026-11-01"),
            Some("2026-11-10"),
        )];
        let span = TargetSpan::new(Some(day("2026-10-28")), Some(day("2026-11-02")));
        let conflict = check_successors(&span, &succs).expect_err("晚于后继开始必须拒");
        assert_eq!(conflict.code, error_code::TASK_SCHEDULE_SUCCESSOR);
        assert_eq!(conflict.code, 63111);
        assert!(
            conflict.message.contains("TSK-202610-000015"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.contains("接口联调"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.contains("2026-11-01"),
            "{}",
            conflict.message
        );
        assert!(
            conflict.message.starts_with("改期违反依赖：后继 "),
            "文案前缀必须是卡面写死的形态：{}",
            conflict.message
        );

        // 正好等于后继开始日 → 允许
        assert!(
            check_successors(
                &TargetSpan::new(Some(day("2026-10-28")), Some(day("2026-11-01"))),
                &succs
            )
            .is_ok()
        );
        // 取消的后继不参与
        let cancelled = vec![peer(
            "TSK-202610-000016",
            "已取消的后继",
            "CANCELLED",
            Some("2026-11-01"),
            Some("2026-11-10"),
        )];
        assert!(
            check_successors(
                &TargetSpan::new(Some(day("2026-10-28")), Some(day("2026-12-01"))),
                &cancelled
            )
            .is_ok()
        );
    }

    // ---------- ④ 父子 ----------

    #[test]
    fn child_outside_parent_range_is_rejected_with_names() {
        let parent = peer(
            "TSK-202610-000001",
            "父任务",
            "DOING",
            Some("2026-11-01"),
            Some("2026-11-30"),
        );
        let conflict = check_parent_range(
            &TargetSpan::new(Some(day("2026-10-28")), Some(day("2026-11-10"))),
            Some(&parent),
        )
        .expect_err("子任务开始早于父任务必须拒");
        assert_eq!(conflict.code, error_code::TASK_SCHEDULE_PARENT_RANGE);
        assert_eq!(conflict.code, 63112);
        assert!(
            conflict.message.contains("TSK-202610-000001"),
            "{}",
            conflict.message
        );
        assert!(conflict.message.contains("父任务"), "{}", conflict.message);
        assert!(
            conflict.message.contains("2026-11-01"),
            "文案必须带父任务区间：{}",
            conflict.message
        );

        // 父任务没排期 → 无区间可违反
        let unscheduled = peer("TSK-202610-000002", "没排期的父任务", "DOING", None, None);
        assert!(
            check_parent_range(
                &TargetSpan::new(Some(day("2026-01-01")), Some(day("2026-01-02"))),
                Some(&unscheduled)
            )
            .is_ok()
        );
    }

    #[test]
    fn parent_shift_must_not_push_children_out() {
        let children = vec![peer(
            "TSK-202610-000021",
            "子任务甲",
            "TODO",
            Some("2026-11-01"),
            Some("2026-11-03"),
        )];
        // 父任务整体 +5 天 → 开始日 11-06，子任务甲还在 11-01（被甩出去）
        let conflict = check_children_range(
            &TargetSpan::new(Some(day("2026-11-06")), Some(day("2026-11-20"))),
            &children,
        )
        .expect_err("父任务不级联，甩出子任务必须拒");
        assert_eq!(conflict.code, 63112);
        assert!(
            conflict.message.contains("TSK-202610-000021"),
            "{}",
            conflict.message
        );
        assert!(conflict.message.contains("不级联"), "{}", conflict.message);

        // 区间包含子任务 → 通过
        assert!(
            check_children_range(
                &TargetSpan::new(Some(day("2026-10-25")), Some(day("2026-11-20"))),
                &children
            )
            .is_ok()
        );
    }

    // ---------- 顺序 ----------

    #[test]
    fn check_order_reports_date_sanity_before_dependencies() {
        let current = row(Some("2026-11-01"), Some("2026-11-05"));
        let span = TargetSpan::new(Some(day("2026-11-09")), Some(day("2026-11-05"))); // 反过来
        let preds = vec![peer(
            "TSK-202610-000012",
            "数据库设计",
            "DOING",
            Some("2026-10-20"),
            Some("2026-10-30"),
        )];
        let conflict = check_all(&span, &current, &preds, &[], None, &[])
            .expect_err("倒置区间必须先被日期合法性拦住");
        assert_eq!(
            conflict.code,
            error_code::TASK_FIELD_INVALID,
            "顺序必须是 ①日期 → ②前置"
        );
    }

    #[test]
    fn conflict_encoding_round_trips() {
        let conflict = ScheduleConflict::new(
            63110,
            "改期违反依赖：前置 X「Y」（截止 Z）尚未完成".to_string(),
        );
        let raw = conflict.encode();
        assert_eq!(ScheduleConflict::decode(&raw), Some(conflict));
        assert!(ScheduleConflict::decode("真正的数据库异常").is_none());
        assert!(ScheduleConflict::decode("TASK_SCHEDULE_CONFLICT:abc|x").is_none());
    }

    #[test]
    fn signed_days_renders_sign() {
        assert_eq!(signed_days(3), "+3 天");
        assert_eq!(signed_days(-2), "-2 天");
        assert_eq!(signed_days(0), "+0 天");
    }
}
