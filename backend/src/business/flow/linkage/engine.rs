//! 跨单据联动：**执行引擎**（第 1 步把它建好，但本卡不接管任何写逻辑）。
//!
//! # 触发点
//!
//! 「单据状态变更之后（走状态机的地方）」——调用方拿着自己的 `&impl SqlExecutor`
//! 和一份 [`LinkageContext`] 调 [`run`]：
//!
//! ```text
//! ① 查 biz_flow_linkage_rule  where trigger_entity=? and trigger_status=? and enabled
//! ② 按 seq 升序（同 seq 按 id 稳定排序）
//! ③ 逐个执行白名单动作（先按 Phase 分两栏，见下）
//! ④ 每个动作的执行结果记 RuleRun（成功 / 成功但跳过 / 失败）+ 结构化日志
//! ⑤ 失败处理：关键动作失败 → Err（调用方整笔回滚）；非关键动作失败 → 记日志继续
//! ```
//!
//! # 事务边界（卡面要求「在卡里说清楚」——这里是唯一权威）
//!
//! | 栏位 | 谁执行 | 在事务里？ | 失败怎么办 |
//! |------|--------|-----------|-----------|
//! | `Phase::InTx` + `Critical`（写库存 / 凭证 / 累计量 / 改状态） | [`run`]，用调用方的 `ex` | **是**（调用方的事务，本模块**不**再开一层） | `run` 返回 `Err` → 调用方 `tx.rollback()`，整笔状态变更一起回滚 |
//! | `Phase::InTx` + `NonCritical` | [`run`] | 是 | 记 `Failed`，流程继续 |
//! | `Phase::AfterCommit`（发通知 / 建任务） | [`run_deferred`]，**提交之后**调 | **否**（各自的事务） | 记 `Failed`，永不回滚业务 |
//!
//! 为什么 `InTx` 不在引擎里再包一层事务：调用方（如 `stockin_handler::post_in_tx`）已经
//! 在事务里，PG 没有嵌套事务；套 `SAVEPOINT` 只能局部回滚，会把「关键动作失败 → 整笔
//! 回滚」偷偷降级成「只有这段回滚」——那正是卡面三令五申不要的东西。
//!
//! 所以调用方的用法固定为两步：
//!
//! ```ignore
//! let run = linkage::engine::run(&tx, &ctx).await?;   // 关键动作在这里；失败即 Err
//! tx.commit().await?;
//! let runs = linkage::engine::run_deferred(&ctx, run.deferred).await;  // 通知这类
//! ```
//!
//! # 第 2 步（SEND_NOTICE 接管）另有一个入口：[`status_notice`]
//!
//! 任务状态变更的通知不是「纯新增联动」，而是**已有逻辑的接管**。为了能「并行跑一段、
//! 对比结果一致」，[`status_notice`] 在配置了 `SEND_NOTICE` 规则时：
//!
//! - `mode = SHADOW`（默认）：把配置算出来的通知与旧逻辑的通知**逐字段对比**并打日志，
//!   通知仍由旧逻辑发（用户可见行为不变）；
//! - `mode = TAKEOVER`：由配置发，旧逻辑不发（对比结果同样落日志，便于事后核对）。
//!
//! 任何一步校验不过（参数非法 / 收件人都不存在 / 发送失败），**一律退回旧逻辑**：
//! 通知漏发是「用户不知道单子卡在他那儿」的事故，不能因为配置写错就丢。

use std::result::Result;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::notice::NoticeDraft;
use crate::shared::error_code;

use super::actions::{self, ActionError};
use super::model::{self, LinkageRuleRow};
use super::spec::{self, Phase, Severity};

/// 一次联动的上下文（触发点提供，引擎不查业务库）。
#[derive(Debug, Clone)]
pub struct LinkageContext<'a> {
    /// 触发单据类型（`spec::TRIGGERS` 之一）。
    pub entity: &'a str,
    /// 触发状态（变更**之后**的状态）。
    pub status: &'a str,
    /// 触发单据主键。
    pub doc_id: Uuid,
    /// 触发单据编号（日志 / 审计可读性）。
    pub doc_no: &'a str,
    /// 操作人。
    pub operator: Option<Uuid>,
    /// 本次时间（一处取值，避免动作之间时间漂移）。
    pub now: DateTime<Utc>,
    /// 负责人（`recipient = OWNER` 时用）。
    pub owner: Option<Uuid>,
    /// 创建人（`recipient = CREATOR` 时用）。
    pub creator: Option<Uuid>,
    /// 分派人（`recipient = ASSIGNER` 时用）。
    pub assigner: Option<Uuid>,
    /// 触发点已有的通知草稿（`SEND_NOTICE` 的基线：配置不覆盖时逐字沿用它）。
    pub notice: Option<NoticeDraft>,
}

impl<'a> LinkageContext<'a> {
    /// 最小上下文（只有触发点 + 单据 + 操作人）。
    pub fn new(
        entity: &'a str,
        status: &'a str,
        doc_id: Uuid,
        doc_no: &'a str,
        operator: Option<Uuid>,
    ) -> Self {
        Self {
            entity,
            status,
            doc_id,
            doc_no,
            operator,
            now: Utc::now(),
            owner: None,
            creator: None,
            assigner: None,
            notice: None,
        }
    }

    /// 追加三个「人」（`recipient` 策略用）。
    pub fn with_people(
        mut self,
        owner: Option<Uuid>,
        creator: Option<Uuid>,
        assigner: Option<Uuid>,
    ) -> Self {
        self.owner = owner;
        self.creator = creator;
        self.assigner = assigner;
        self
    }

    /// 附上触发点已有的通知草稿。
    pub fn with_notice(mut self, draft: NoticeDraft) -> Self {
        self.notice = Some(draft);
        self
    }
}

/// 单个动作的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 执行成功（附人读说明）。
    Success(String),
    /// 校验不过 / 尚未接管 → 跳过（不中断流程）。
    Skipped(String),
    /// 执行失败（按 `severity` 决定回滚还是继续）。
    Failed(String),
}

impl Outcome {
    /// 是否成功。
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Success(_))
    }

    /// 简明标签（日志 / 测试断言用）。
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Success(_) => "SUCCESS",
            Outcome::Skipped(_) => "SKIPPED",
            Outcome::Failed(_) => "FAILED",
        }
    }

    /// 人读说明。
    pub fn detail(&self) -> &str {
        match self {
            Outcome::Success(note) | Outcome::Skipped(note) | Outcome::Failed(note) => note,
        }
    }
}

/// 一条规则的执行记录（= 卡面要求的「每个动作执行结果写日志」）。
#[derive(Debug, Clone)]
pub struct RuleRun {
    /// 规则主键。
    pub rule_id: Uuid,
    /// 规则名。
    pub rule_name: String,
    /// 动作码。
    pub action_code: String,
    /// 执行顺序。
    pub seq: i32,
    /// 结果。
    pub outcome: Outcome,
}

/// 一次联动执行的总结果。
#[derive(Debug, Clone, Default)]
pub struct LinkageRun {
    /// 已执行（或跳过）的规则，顺序 = 执行顺序。
    pub runs: Vec<RuleRun>,
    /// 待提交后执行的规则（`Phase::AfterCommit`，按 seq 升序）。
    pub deferred: Vec<LinkageRuleRow>,
}

impl LinkageRun {
    /// 是否一条规则都没命中（= 行为与配置前逐字相同）。
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.deferred.is_empty()
    }
}

/// 执行**事务内**动作（`Phase::InTx`）。
///
/// # 返回
///
/// - `Ok(run)`：全部非关键动作已完成/跳过；`run.deferred` 待提交后交给 [`run_deferred`]。
/// - `Err(ApiError)`：**关键动作执行失败**——调用方必须回滚当前事务。
pub async fn run(ex: &impl SqlExecutor, ctx: &LinkageContext<'_>) -> Result<LinkageRun, ApiError> {
    let rules = model::rules_for(ex, ctx.entity, ctx.status).await;
    let mut runs: Vec<RuleRun> = Vec::with_capacity(rules.len());
    let mut deferred: Vec<LinkageRuleRow> = Vec::new();

    for rule in rules {
        let Some(action) = rule.spec() else {
            runs.push(record(
                &rule,
                Outcome::Skipped(format!(
                    "动作码 {} 不在白名单里（数据库 CHECK 之外的历史脏行），跳过",
                    rule.action_code
                )),
                ctx,
            ));
            continue;
        };
        if !action.implemented {
            // 占位动作：配置写入阶段就会被拒，这里兜住「绕过 API 直接写库」的情况
            runs.push(record(
                &rule,
                Outcome::Skipped(format!(
                    "动作 {} 尚未接管（阶段 3）：占位动作不执行任何写入",
                    rule.action_code
                )),
                ctx,
            ));
            continue;
        }
        // 第 3 层：执行前再校验一次参数（配置可能被绕过 API 改动）
        if let Err(reason) = spec::validate_params(action, &rule.action_params) {
            runs.push(record(&rule, Outcome::Skipped(reason), ctx));
            continue;
        }
        match action.phase {
            Phase::AfterCommit => deferred.push(rule),
            Phase::InTx => match actions::run_in_tx(ex, &rule, ctx).await {
                Ok(note) => {
                    runs.push(record(&rule, Outcome::Success(note), ctx));
                }
                Err(ActionError::Skipped(reason)) => {
                    runs.push(record(&rule, Outcome::Skipped(reason), ctx));
                }
                Err(ActionError::Failed(reason)) => {
                    let critical = action.severity == Severity::Critical;
                    runs.push(record(&rule, Outcome::Failed(reason.clone()), ctx));
                    if critical {
                        return Err(ApiError::new(
                            500,
                            error_code::INTERNAL_ERROR,
                            format!(
                                "联动动作 {}（{}）失败：{reason}。这是关键动作，\
                                 本次状态变更已整笔回滚（不会留下半条账）",
                                rule.action_code, rule.name
                            ),
                        ));
                    }
                }
            },
        }
    }

    Ok(LinkageRun { runs, deferred })
}

/// 执行**提交后**动作（`Phase::AfterCommit`）。
///
/// **永不返回 Err**：通知/建任务失败只记日志（`Outcome::Failed`），
/// 业务已经在库里落定，回滚它等于「用户点了没反应」。
pub async fn run_deferred(ctx: &LinkageContext<'_>, deferred: Vec<LinkageRuleRow>) -> Vec<RuleRun> {
    let mut runs = Vec::with_capacity(deferred.len());
    for rule in deferred {
        let outcome = match rule.spec() {
            None => Outcome::Skipped(format!(
                "动作码 {} 不在白名单里（历史脏行），跳过",
                rule.action_code
            )),
            Some(action) if !action.implemented => Outcome::Skipped(format!(
                "动作 {} 尚未接管（阶段 3）：占位动作不执行",
                rule.action_code
            )),
            Some(action) => match spec::validate_params(action, &rule.action_params) {
                Err(reason) => Outcome::Skipped(reason),
                Ok(()) => match actions::run_after_commit(&rule, ctx).await {
                    Ok(note) => Outcome::Success(note),
                    Err(ActionError::Skipped(reason)) => Outcome::Skipped(reason),
                    Err(ActionError::Failed(reason)) => Outcome::Failed(reason),
                },
            },
        };
        runs.push(record(&rule, outcome, ctx));
    }
    runs
}

/// 记一条执行日志并返回执行记录。
///
/// 成功打 `INFO`，跳过/失败打 `ERROR`（卡面：运行阶段不过 → ERROR 日志 + 跳过）。
/// 字段名固定（`trigger_entity/trigger_status/doc_id/doc_no/rule_id/action_code/seq/outcome`），
/// 便于 `grep` 与日志平台按字段过滤。
fn record(rule: &LinkageRuleRow, outcome: Outcome, ctx: &LinkageContext<'_>) -> RuleRun {
    let label = outcome.label();
    let reason = outcome.detail();
    if outcome.is_success() {
        tracing::info!(
            trigger_entity = ctx.entity,
            trigger_status = ctx.status,
            doc_id = %ctx.doc_id,
            doc_no = ctx.doc_no,
            rule_id = %rule.id,
            action_code = rule.action_code,
            seq = rule.seq,
            outcome = label,
            reason = reason,
            "联动动作执行结果"
        );
    } else {
        tracing::error!(
            trigger_entity = ctx.entity,
            trigger_status = ctx.status,
            doc_id = %ctx.doc_id,
            doc_no = ctx.doc_no,
            rule_id = %rule.id,
            action_code = rule.action_code,
            seq = rule.seq,
            outcome = label,
            reason = reason,
            "联动动作执行结果"
        );
    }
    RuleRun {
        rule_id: rule.id,
        rule_name: rule.name.clone(),
        action_code: rule.action_code.clone(),
        seq: rule.seq,
        outcome,
    }
}

// ==================== 第 2 步：SEND_NOTICE 的接管入口 ====================

/// 影子对比结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowCompare {
    /// 是否与触发点旧逻辑算出来的通知完全一致。
    pub matched: bool,
    /// 不一致的字段名（空 = 一致）。
    pub diffs: Vec<&'static str>,
}

/// 通知该由谁发。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeRoute {
    /// 旧逻辑发（`compare` 非空表示「配置已参与但只做影子对比」）。
    Legacy {
        /// 影子对比结果（`None` = 压根没有 `SEND_NOTICE` 配置）。
        compare: Option<ShadowCompare>,
    },
    /// 配置接管：旧逻辑**不再**发。
    Configured {
        /// 生效的规则。
        rule_id: Uuid,
        /// 是否真的发出去了（`false` = 发送失败，但已无旧逻辑兜底）。
        dispatched: bool,
        /// 影子对比结果。
        compare: ShadowCompare,
    },
}

/// 任务/单据状态变更时的通知路由（第 2 步的接管点）。
///
/// 调用方（`task::handler::notify_status_change`）已经算好了「旧逻辑要发的草稿」并放进
/// [`LinkageContext::notice`]；这里决定「这一条通知到底谁来发」。
///
/// 语义（每一步失败都退回旧逻辑，绝不丢通知）：
///
/// 1. 没有 `SEND_NOTICE` 规则 → [`NoticeRoute::Legacy`]（`compare = None`）；
/// 2. 规则整形失败（参数非法 / 收件人都没了）→ 打 ERROR，继续试下一条，最终 [`NoticeRoute::Legacy`]；
/// 3. 整形成功 → 与旧草稿逐字段对比并打日志（`matched` / `diffs`）；
/// 4. `mode = SHADOW` → [`NoticeRoute::Legacy`]（`compare = Some`），旧逻辑照发；
/// 5. `mode = TAKEOVER` → 由配置发；发送失败 → 打 ERROR 并退回 [`NoticeRoute::Legacy`]。
pub async fn status_notice(ctx: &LinkageContext<'_>) -> NoticeRoute {
    let Some(base) = ctx.notice.clone() else {
        return NoticeRoute::Legacy { compare: None };
    };
    let rules = model::rules_for(db(), ctx.entity, ctx.status).await;
    let candidates: Vec<LinkageRuleRow> = rules
        .into_iter()
        .filter(|rule| rule.action_code == spec::ACTION_SEND_NOTICE && rule.enabled)
        .collect();
    if candidates.is_empty() {
        return NoticeRoute::Legacy { compare: None };
    }

    let mut last_compare: Option<ShadowCompare> = None;
    for rule in candidates {
        // 占位/脏行不应该出现在这里（ACTION_SEND_NOTICE 已实现），仍然防御一次
        let Some(action) = rule.spec() else {
            tracing::error!(
                rule_id = %rule.id,
                action_code = rule.action_code,
                "联动规则的动作码不在白名单里，跳过（通知仍由旧逻辑发）"
            );
            continue;
        };
        if !action.implemented {
            tracing::error!(
                rule_id = %rule.id,
                action_code = rule.action_code,
                "SEND_NOTICE 尚未接管，跳过（通知仍由旧逻辑发）"
            );
            continue;
        }
        // 第 3 层：参数复校（配置可能被绕过 API 改动）
        if let Err(reason) = spec::validate_params(action, &rule.action_params) {
            tracing::error!(
                rule_id = %rule.id,
                trigger_entity = ctx.entity,
                trigger_status = ctx.status,
                doc_id = %ctx.doc_id,
                reason = reason,
                "联动 SEND_NOTICE 参数非法：跳过该规则（通知仍由旧逻辑发，不会丢）"
            );
            continue;
        }
        let (draft, setting) = match actions::prepare_notice(&rule, ctx).await {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::error!(
                    rule_id = %rule.id,
                    trigger_entity = ctx.entity,
                    trigger_status = ctx.status,
                    doc_id = %ctx.doc_id,
                    reason = error.reason(),
                    "联动 SEND_NOTICE 校验不过：跳过该规则（通知仍由旧逻辑发，不会丢）"
                );
                continue;
            }
        };
        let diffs = actions::compare_notices(&draft, &base);
        let compare = ShadowCompare {
            matched: diffs.is_empty(),
            diffs: diffs.clone(),
        };
        tracing::info!(
            rule_id = %rule.id,
            mode = if setting.mode == actions::NoticeMode::Takeover { "TAKEOVER" } else { "SHADOW" },
            trigger_entity = ctx.entity,
            trigger_status = ctx.status,
            doc_id = %ctx.doc_id,
            doc_no = ctx.doc_no,
            matched = compare.matched,
            diffs = ?compare.diffs,
            "联动影子对比：配置算出的通知 vs 触发点旧逻辑"
        );
        last_compare = Some(compare.clone());

        if setting.mode != actions::NoticeMode::Takeover {
            // 影子模式：旧逻辑发
            continue;
        }
        match actions::dispatch_notice(draft).await {
            Ok(outcome) => {
                tracing::info!(
                    rule_id = %rule.id,
                    notice_id = %outcome.notice_id,
                    receiver_count = outcome.receiver_count,
                    "联动已接管通知：配置发出（旧逻辑本次不发）"
                );
                return NoticeRoute::Configured {
                    rule_id: rule.id,
                    dispatched: true,
                    compare,
                };
            }
            Err(error) => {
                tracing::error!(
                    rule_id = %rule.id,
                    reason = error.reason(),
                    "联动接管通知发送失败：退回旧逻辑（不丢通知）"
                );
                continue;
            }
        }
    }

    NoticeRoute::Legacy {
        compare: last_compare,
    }
}
