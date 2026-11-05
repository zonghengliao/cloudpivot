// ============================================================================
// task_handler_notice.rs —— 通知扇出：草稿拼装 / 分级发送 / 状态变更与前置联动提醒
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::uuid::Uuid;

use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::task::dep;
use crate::business::task::model::{self, TaskDetailRow};
use crate::business::task::state::TaskStatus;

/// 一条任务通知的内容（把「收件人 / 强度 / 文案」打包，避免 8 个位置参数）。
///
/// 单独抽结构的直接原因：clippy `too_many_arguments`（8/7）在 `-D warnings` 下是硬错误；
/// 顺带的收益是调用点可读 —— `TaskNotice { recipients, force, .. }` 比 8 个裸参数
/// 更难把 `force` 和 `exclude_operator` 两个布尔写反（它们语义完全不同）。
pub(super) struct TaskNotice<'a> {
    /// 候选收件人（会去掉操作人自己、排序去重）。
    /// 字段可见性 `pub(super)` 仅为跨部件构造（拆分前同文件内私有）。
    pub(super) recipients: Vec<Uuid>,
    /// `true` = 强制送达（跳过 `notify.inbox` 偏好）。
    pub(super) force: bool,
    /// 是否剔除操作人自己（转派给原负责人的回执要传 `false`）。
    pub(super) exclude_operator: bool,
    /// 标题。
    pub(super) title: &'a str,
    /// 正文。
    pub(super) content: &'a str,
}

/// 发一条任务站内信（**唯一的任务通知出口**）。
///
/// # 参数
///
/// - `task`：任务（取标题 / 编号 / 主键拼链接）；
/// - `recipients`：候选接收人（会自动去掉操作人自己、去重）；
/// - `force`：`true` 时**跳过** `notify.inbox` 偏好（修正 8 的强制分级）。
///   `false` 时关掉站内信的人不会落 `sys_notice_receiver`（过滤在
///   `notice::dispatch` 里做，本函数只传开关）；
/// - `exclude_operator`：是否剔除操作人自己（默认 `true` —— 「自己干的事不用通知自己」）。
///   **转派时给原负责人的那条回执必须传 `false`**：原负责人往往就是操作人，
///   而卡面明确要求「原负责人收到『任务已被转派』」（这是「你的活确实交出去了」的凭据，
///   不是噪音）；
///
/// - `title` / `content`：文案；
/// - `operator` / `operator_name`：发送人（通知里显示「谁干的」）。
///
/// # 返回值
///
/// **实际投递的目标**（已剔除操作人自己；通知失败时为空 `Vec`）。
/// 分派接口用它回填 `notified`，便于调用方确认「谁真的被告知了」。
///
/// # 失败口径
///
/// **通知失败不影响业务**：状态已经落库、评论已经写好，这里只记 WARN。
/// 把通知当强一致会让「通知服务抖一下 → 任务改不了」。
pub(super) async fn dispatch_task_notice(
    task: &TaskDetailRow,
    notice: TaskNotice<'_>,
    operator: Uuid,
    operator_name: &str,
) -> Vec<Uuid> {
    match task_notice_draft(task, notice, operator, operator_name) {
        Some(draft) => send_task_notice_draft(draft).await,
        None => Vec::new(),
    }
}

/// 组装任务站内信草稿（**不发**）。
///
/// 抽出来的唯一原因是「跨单据联动白名单」（D2 · 阶段 4）：配置驱动的通知要与
/// 这条既有通知**逐字段对比**（影子模式）才能证明接管后行为一致。
/// 文案、收件人、链接、分级仍然只有这一处计算 —— 配置不写覆盖键时逐字沿用它，
/// 所以不存在「两套文案」的双真相。
pub(super) fn task_notice_draft(
    task: &TaskDetailRow,
    notice: TaskNotice<'_>,
    operator: Uuid,
    operator_name: &str,
) -> Option<NoticeDraft> {
    let TaskNotice {
        recipients,
        force,
        exclude_operator,
        title,
        content,
    } = notice;
    let mut targets: Vec<Uuid> = recipients
        .into_iter()
        .filter(|user_id| !(exclude_operator && *user_id == operator))
        .collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return None;
    }
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        title,
        content,
        NoticeTarget::User(targets),
    );
    draft.link_url = Some(format!("/task/list?id={}", task.card.id));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(task.card.id.to_string());
    draft.sender_id = Some(operator);
    draft.sender_name = Some(operator_name.to_string());
    // 修正 8 的分级：**只管把 force 传下去**，偏好过滤的唯一实现在
    // `notice::dispatch` → `fanout::respecting_preference`（本卡把 T2 里那份
    // Rust 侧逐个 `inbox_enabled` 的判断删掉了：两份实现迟早不一致）。
    draft.force = force;
    Some(draft)
}

/// 投递一份已组装的任务站内信，返回实际投递目标（失败 → 空）。
pub(super) async fn send_task_notice_draft(draft: NoticeDraft) -> Vec<Uuid> {
    let targets = match &draft.target {
        NoticeTarget::User(users) => users.clone(),
        _ => Vec::new(),
    };
    let force = draft.force;
    match notice::dispatch(draft).await {
        Ok(result) => {
            tracing::info!(
                notice_id = %result.notice_id,
                receivers = result.receiver_count,
                force,
                "任务通知已发送"
            );
            targets
        }
        Err(error) => {
            tracing::warn!(error = %error, force, "任务通知发送失败（不影响业务）");
            Vec::new()
        }
    }
}

/// 状态流转通知（修正 8 分级表，逐行实现；**D2 起可被配置接管**）。
///
/// | 事件 | 接收人 | 强制 |
/// |---|---|---|
/// | 被打回（`REVIEW → DOING`） | 负责人 | ✅ |
/// | 待你验收（`→ REVIEW`） | 创建人 + 分派人 | ✅ |
/// | 其它状态变更（他人操作） | 负责人 + 创建人 | ❌ 尊重 `notify.inbox` |
///
/// # 与跨单据联动白名单的关系（D2 · 阶段 4 · 第 2 步）
///
/// 通知的**内容**（收件人 / 分级 / 文案）仍由上面这张表算；配置能改的只是
/// 「由谁发、发不发、覆盖哪些字段」：
///
/// 1. 先用本函数算好的草稿问一句 `flow::linkage::engine::status_notice`；
/// 2. 没有 `SEND_NOTICE` 规则 → 原样照旧发（**行为逐字不变**）；
/// 3. 规则 `mode = SHADOW` → 打一条「影子对比」日志，仍由这里发（用户看不到任何变化）；
/// 4. 规则 `mode = TAKEOVER` → 引擎发了，这里**不发**（否则用户会收到两条一样的）。
///
/// 任何一步校验不过（参数非法 / 收件人都不存在），引擎一律退回「旧逻辑发」——
/// 通知漏发是事故，配置写错不能让它变成事故。
pub(super) async fn notify_status_change(
    task: &TaskDetailRow,
    from: TaskStatus,
    to: TaskStatus,
    operator: Uuid,
    operator_name: &str,
    reason: Option<&str>,
) {
    let (recipients, force, title, content) = match (from, to) {
        (TaskStatus::Review, TaskStatus::Doing) => (
            vec![task.card.owner_id],
            true,
            "任务被打回".to_string(),
            format!(
                "任务「{}」（{}）被打回：{}；改完请重新提交验收",
                task.card.title,
                task.card.task_no,
                reason.unwrap_or("未填写原因")
            ),
        ),
        (_, TaskStatus::Review) => {
            let mut recipients = vec![task.creator_id];
            if let Some(assigner_id) = task.assigner_id {
                recipients.push(assigner_id);
            }
            (
                recipients,
                true,
                "任务待你验收".to_string(),
                format!(
                    "任务「{}」（{}）已由 {operator_name} 提交验收，请确认",
                    task.card.title, task.card.task_no
                ),
            )
        }
        _ => (
            vec![task.card.owner_id, task.creator_id],
            false,
            "任务状态变更".to_string(),
            format!(
                "任务「{}」（{}）由 {operator_name} 从「{}」改为「{}」",
                task.card.title,
                task.card.task_no,
                from.label(),
                to.label()
            ),
        ),
    };
    let Some(draft) = task_notice_draft(
        task,
        TaskNotice {
            recipients,
            force,
            exclude_operator: true,
            title: &title,
            content: &content,
        },
        operator,
        operator_name,
    ) else {
        return;
    };
    // 跨单据联动白名单（D2 · 阶段 4 · 第 2 步）：有 SEND_NOTICE 配置时决定「谁发」
    // （影子对比 / 接管），没配置时恒返回 Legacy —— 行为与 D2 之前逐字相同。
    let ctx = crate::business::flow::linkage::engine::LinkageContext::new(
        crate::business::flow::linkage::spec::TRIGGER_TASK,
        to.code(),
        task.card.id,
        &task.card.task_no,
        Some(operator),
    )
    .with_people(
        Some(task.card.owner_id),
        Some(task.creator_id),
        task.assigner_id,
    )
    .with_notice(draft.clone());
    match crate::business::flow::linkage::engine::status_notice(&ctx).await {
        crate::business::flow::linkage::NoticeRoute::Legacy { .. } => {
            send_task_notice_draft(draft).await;
        }
        crate::business::flow::linkage::NoticeRoute::Configured { rule_id, .. } => {
            tracing::info!(
                task_id = %task.card.id,
                task_no = %task.card.task_no,
                status = to.code(),
                rule_id = %rule_id,
                "任务状态通知已由联动配置接管（旧逻辑本次不发）"
            );
        }
    }
}

/// 父任务「子任务全部完成」通知（尊重偏好，修正 8）。
///
/// 只在**这次重算把父任务推到 100%** 时发一次（`old != 100 && new == 100`），
/// 避免同一次批量流转里对同一个父任务重复通知。
pub(super) async fn notify_ready_to_close(
    recalcs: &[model::ProgressRecalc],
    operator: Uuid,
    operator_name: &str,
) {
    for recalc in recalcs {
        if !recalc.all_done || recalc.new != 100 || recalc.old == 100 {
            continue;
        }
        let Ok(Some(parent)) = model::find_raw(recalc.task_id).await else {
            continue;
        };
        let content = format!(
            "任务「{}」（{}）的子任务已全部完成，进度汇总为 100%，可以结项了",
            parent.card.title, parent.card.task_no
        );
        dispatch_task_notice(
            &parent,
            TaskNotice {
                recipients: vec![parent.card.owner_id],
                force: false,
                exclude_operator: true,
                title: "子任务全部完成",
                content: &content,
            },
            operator,
            operator_name,
        )
        .await;
    }
}

/// 前置完成 / 取消 → 通知**后继负责人**（T4 联动，核心设计 §四「运行时约束 4」+ §九通知表）。
///
/// # 分级（修正 8）
///
/// | 事件 | 收件人 | `force` |
/// |---|---|---|
/// | 前置 `DONE`（解开阻塞） | 每个后继的负责人 | `true`（**强制**：这是「可以开工了」的信号，不能被偏好静默丢弃） |
/// | 前置 `CANCELLED` | 每个后继的负责人 | `false`（噪音档：前置取消只是提醒你调整计划） |
///
/// # 口径
///
/// - 只通知**还没结束**（非 `DONE` / `CANCELLED`）的后继：给已完成的任务发「可以开始了」是纯噪音；
/// - 每个后继发**一条**站内信（链到后继任务，不是前置任务 —— 收件人要处理的是自己那条）；
/// - 通知失败只记 WARN，不影响业务（与其它任务通知同一口径）。
pub(super) async fn notify_dependents(
    task: &TaskDetailRow,
    to: TaskStatus,
    operator: Uuid,
    operator_name: &str,
) {
    let successors = match model::list_successors(task.card.id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, task_id = %task.card.id, "查询后继任务失败，跳过依赖联动通知");
            return;
        }
    };
    for successor in successors {
        if !dep::is_blocking_status(&successor.status) {
            continue; // 后继已结束（DONE / CANCELLED），没有必要提醒
        }
        let Ok(Some(target)) = model::find_raw(successor.peer_id).await else {
            continue;
        };
        let (title, content, force) = match to {
            TaskStatus::Done => (
                "前置任务已完成，可以开始了",
                format!(
                    "前置任务「{}」（{}）已完成，任务「{}」（{}）可以开始了",
                    task.card.title, task.card.task_no, target.card.title, target.card.task_no
                ),
                true,
            ),
            _ => (
                "前置任务已取消",
                format!(
                    "前置任务「{}」（{}）已取消 —— 该前置不再阻塞你，但你可能需要调整任务「{}」（{}）的计划",
                    task.card.title, task.card.task_no, target.card.title, target.card.task_no
                ),
                false,
            ),
        };
        dispatch_task_notice(
            &target,
            TaskNotice {
                recipients: vec![target.card.owner_id],
                force,
                exclude_operator: true,
                title,
                content: &content,
            },
            operator,
            operator_name,
        )
        .await;
    }
}

// ==================== 分派 / 转派 / 协作人（T3） ====================
