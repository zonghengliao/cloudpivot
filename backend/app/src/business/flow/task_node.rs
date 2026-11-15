//! `TASK_NODE` 生成工作任务（P-FLOW-1A · 设计 §7.1 的「生成任务的核心实现框架」）。
//!
//! **本文件负责「造什么」，不负责落库**（P2-38）：负责人解析、模板渲染、截止日与优先级
//! 全在这里算；`biz_task` 的写入走 `flow::task_port` 的接缝，实现在中立接线域
//! `business::taskflow`（原「`biz_task` 在流程侧唯一写入口」已随之搬走）。
//!
//! 引擎 `step()` 的 `NodeType::Task` 分支只调 [`spawn`]，`flow` 里不许再出现对 `biz_task`
//! 的写入（闸门 `flow::task_independence_tests` 基线 0）。
//!
//! # 六条纪律（设计 §7.1 逐条落地）
//!
//! 1. **幂等**：先查 `(instance, node_key, round)` 是否已有任务；有则直接返回
//!    —— 唯一索引 `uk_biz_task_flow_node` 是最终依据，`23505` 当幂等成功
//!    （与 `engine.rs` 的 `is_pending_duplicate` 同款）。
//! 2. **失败 → 整笔回滚**：生成任务不是「副作用」，是业务事实。模板渲染失败 / 标题超长 /
//!    派不出人，都让实例发起一起回滚（验收 7：`biz_task` 0 行、`biz_flow_instance` 0 行）。
//! 3. **多人解析 → 1 个 owner + 其余挂 `biz_task_member`**（决策 5）：
//!    N 张任务会让任务列表炸开，只留第一个又会让其余人看不见 —— 都不行。
//!    任务状态机的角色判定只认 `owner/creator/assigner`，协作人「看得见但推不动」，
//!    正好落在现有语义里，零改造。
//! 4. **`task_no` 在同一事务内取**（`pg_advisory_xact_lock` 按月串行化）；
//!    并发撞 `uk_biz_task_task_no` 时重试一次（设计 §8.11）。
//! 5. **`source` 固定 `'FLOW'`，`creator = 流程发起人`**（谁发起谁验收）。
//! 6. **写 changelog**：谁、什么时候、因为哪个流程节点创建了这张任务。
//!
//! # 为什么不写 `biz_task.parent_id`（决策 4）
//!
//! `parent_id` 是 `REFERENCES biz_task(id)` 的自引用外键，语义是「子任务」。
//! 拿它指向流程实例会：① 类型能过但 FK 当场失败；② 即使去掉 FK，任务的
//! `count_open_children` / `progress_locked` / `blocks_child_advance` 三处逻辑会立刻误判
//! （第 1 个任务永远 DONE 不了，因为它「要等第 5 个完成」）。所以改用
//! `flow_instance_id` / `flow_node_key` / `flow_round` 三个专用列 + `source='FLOW'`。

use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use serde_json::Value as Json;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::assignee::{self, InstanceCtx, Resolution};
use crate::business::flow::graph::{EvalContext, SnapshotNode};
use crate::business::flow::model;
use crate::business::flow::node_config::{self, DueConfig, DueMode, DueUnit, TaskNodeConfig};
use crate::business::flow::task_port::{self, SpawnTaskSpec};
use crate::business::flow::template_vars;
use crate::business::notice::dto::ContentFormat;
use crate::business::notice::render;
use cloudpivot_kernel::shared::error_code;

/// `biz_task.source` 的流程值（`V0177` 扩的第 5 个值）。
pub const FLOW_SOURCE: &str = "FLOW";

/// CST 偏移（与 `task::recurring` / `erp::serial_model` 同口径）。
const CST_OFFSET_HOURS: i64 = 8;

/// 生成结果（引擎照它决定「节点转 ACTIVE」还是「节点 SKIPPED 继续往下」）。
#[derive(Debug)]
pub enum SpawnOutcome {
    /// 生成了 1 张任务（节点应转 `ACTIVE` 等人）。
    Created {
        /// 任务 id。
        task_id: Uuid,
        /// 任务编号（`TSK-YYYYMM-NNNNNN`）。
        task_no: String,
        /// 负责人（多人解析里的第一个）。
        owner_id: Uuid,
        /// 协作人数（其余解析出来的人）。
        members: usize,
        /// 截止日（可能为空）。
        due_date: Option<NaiveDate>,
    },
    /// 幂等命中：同一实例 + 同一节点 + 同一轮已经有任务了（**不发第二条**）。
    AlreadyExists {
        /// 已有任务的 id（理论上一定有；查不到时为 `None`）。
        task_id: Option<Uuid>,
    },
    /// `empty_assignee=SKIP` 且配置了 `allow_skip_when_no_assignee=true`：不生成任务（节点跳过）。
    Skipped {
        /// 说明（写日志用）。
        reason: String,
    },
    /// 业务失败：引擎把它变成 `Step::Fail`，**整笔回滚**（不静默降级）。
    ///
    /// # 为什么不用 `Err(ApiError)`
    ///
    /// `DbError` → `ApiError` 的转换是有损的（`utils::db_error` 会把原因折成 500 文案），
    /// 而 `65039` / `65018` 这类业务码必须原样传到前端。仓库里 `assignee::resolve` 用的是
    /// 同一个形态：业务拒绝放在 `Ok(…).finalized.error` 里，DB 异常才走 `Err`。
    Failed(ApiError),
}

/// TASK_NODE 被激活时生成任务（**必须在引擎事务内调用**）。
///
/// # 参数
///
/// - `ex`：引擎事务；
/// - `instance`：流程实例行；
/// - `spec`：快照节点（`node_type = TASK_NODE`）；
/// - `round`：节点轮次（`biz_flow_instance_node.round`）；
/// - `cfg`：已解析的 `config.task`；
/// - `ctx` / `eval`：与审批节点**同一份**指派上下文与求值上下文（决策 3：不造第二套规则）。
///
/// # 返回值
///
/// [`SpawnOutcome`]：`Created` 让节点转 `ACTIVE`；`AlreadyExists` / `Skipped` 让引擎继续推进。
///
/// # 异常情况
///
/// - `400 + 65039`：渲染后标题超长 / 描述超长 / 模板缺变量；
/// - `400 + 65018`：解析出 0 个负责人且 `empty_assignee != SKIP`；
/// - `400 + 65019` / `65032` / `65033`：指派解析自身的业务拒绝（原样透传）；
/// - 数据库异常（含「重试后仍撞任务号」）→ [`DbError`]，整笔回滚。
pub async fn spawn(
    ex: &Transaction,
    instance: &model::InstanceRow,
    spec: &SnapshotNode,
    round: i32,
    cfg: &TaskNodeConfig,
    ctx: &InstanceCtx,
    eval: &EvalContext,
) -> DbResult<SpawnOutcome> {
    // ① 幂等前置查询：同一实例 + 同一节点 + 同一轮已经有任务 → 直接返回。
    //    唯一索引仍是最终依据（并发下靠 23505 兜底），这里只是省掉一次无效渲染。
    //    P2-38：查询走 `task_port` 的接缝（`flow` 不再直接读 `biz_task`）。
    if let Some(existing) = task_port::task_of_node(ex, instance.id, &spec.key, round).await? {
        tracing::info!(
            instance_id = %instance.id,
            node_key = %spec.key,
            round,
            task_id = %existing,
            "任务节点已生成过任务（幂等命中，不发第二条）"
        );
        return Ok(SpawnOutcome::AlreadyExists {
            task_id: Some(existing),
        });
    }

    // ② 解析负责人（**复用 assignee::resolve**，决策 3）。
    //
    // `self_approval` 对任务节点**不成立**（设计 §2.4 的列语义表：`self_approval` 在
    // TASK_NODE 上「忽略」）。而共用的 `resolve()` 会把「候选 == 发起人」按自审策略
    // SKIP 掉 —— 对审批是对的（自己不能批自己），对干活是**错的**（自己把活派给自己
    // 是最常见的情形，SKIP 会让「来源=发起人」的任务节点永远解析出 0 人 → 65018）。
    // 所以这里把策略改成 ALLOW 再交给同一个解析器：**不造第二套指派规则**，
    // 只是告诉它「本节点没有自审这个概念」。
    let mut work_spec = spec.clone();
    work_spec.self_approval = "ALLOW".to_string();
    let Resolution { finalized, trace } = assignee::resolve(ex, &work_spec, ctx, eval).await?;
    if let Some(error) = finalized.error {
        // 业务拒绝（65018 没负责人 / 65019 自审禁止 / 65033 表达式保留词）→ 透传给引擎，
        // 引擎把它变成 Step::Fail，整笔回滚（**不静默降级**）。
        tracing::warn!(
            instance_id = %instance.id,
            node_key = %spec.key,
            code = error.code,
            msg = %error.msg,
            "任务节点的负责人解析失败"
        );
        return Ok(SpawnOutcome::Failed(error));
    }
    if finalized.approvers.is_empty() {
        // 解析出 0 人：SKIP 需要 `allow_skip_when_no_assignee=true`（R20 已保证发布期拦过，
        // 这里再判一次是因为「旧快照 / 直接改库」绕得过发布校验）。
        if spec.empty_assignee.eq_ignore_ascii_case("SKIP") && cfg.allow_skip_when_no_assignee {
            tracing::warn!(
                instance_id = %instance.id,
                node_key = %spec.key,
                "任务节点解析出 0 个负责人，且配置允许跳过 → 不生成任务"
            );
            return Ok(SpawnOutcome::Skipped {
                reason: format!(
                    "节点「{}」解析不出负责人（来源 {}），按配置跳过",
                    spec.name,
                    spec.assignee_type.as_deref().unwrap_or("（未配置）")
                ),
            });
        }
        return Ok(SpawnOutcome::Failed(ApiError::new(
            400,
            error_code::FLOW_NO_ASSIGNEE,
            format!(
                "节点「{}」解析不出负责人（来源 {} 命中 0 人）：任务节点不能没有负责人，请检查指派配置",
                spec.name,
                spec.assignee_type.as_deref().unwrap_or("（未配置）")
            ),
        )));
    }

    // ③ 模板变量 → 渲染标题 / 描述（缺变量 → 65039，整笔回滚）。
    let vars = template_vars::build(ex, instance, spec, round, true).await?;
    for warning in &vars.warnings {
        tracing::warn!(instance_id = %instance.id, node_key = %spec.key, "{warning}");
    }
    let title = match render_title(&vars, cfg) {
        Ok(title) => title,
        Err(error) => return Ok(SpawnOutcome::Failed(error)),
    };
    let description = match cfg.description_template.as_deref() {
        Some(template) => match render_description(&vars, template) {
            Ok(text) => Some(text),
            Err(error) => return Ok(SpawnOutcome::Failed(error)),
        },
        None => None,
    };

    // ④ 负责人：**第一个**（按 user_id 排序，保证「同一份配置、同一批人」结果可复现），
    //    其余挂协作人（决策 5）。
    let mut approvers = finalized.approvers;
    approvers.sort_by(|left, right| left.user_id.cmp(&right.user_id));
    let owner = approvers.remove(0);
    let members = approvers;

    // ⑤ 截止日 / 优先级 / 预估工时（`config.task.due` + `form_mapping`）。
    //
    // P2-38：到这里为止全部是**流程侧的计算**（负责人解析 / 模板渲染 / 日期与优先级），
    // 下面把算好的字面量交给 `task_port` 去落库 —— `flow` 不再出现 `biz_task` 的读写，
    // 也不会 `import task::model`（闸门 `task_independence_tests` 基线 0）。
    let due_date = resolve_due_date(instance, cfg);
    let priority = resolve_priority(&vars.dotted, cfg);
    let estimate_hours = resolve_estimate_hours(&vars.dotted, cfg);

    // ⑥ 落库：取号 + 插任务 + 挂协作人 + 变更日志（全在接线域，同一个事务）。
    let outcome = task_port::spawn_task(
        ex,
        SpawnTaskSpec {
            instance_id: instance.id,
            node_key: spec.key.clone(),
            node_name: spec.name.clone(),
            round,
            title,
            description,
            priority,
            need_review: cfg.need_review,
            owner_id: owner.user_id,
            member_ids: members.iter().map(|candidate| candidate.user_id).collect(),
            creator_id: instance.initiator_id,
            creator_name: instance.initiator_name.clone(),
            due_date,
            estimate_hours,
        },
    )
    .await?;

    if let SpawnOutcome::Created {
        task_id,
        ref task_no,
        owner_id,
        members,
        due_date,
    } = outcome
    {
        tracing::info!(
            instance_id = %instance.id,
            node_key = %spec.key,
            round,
            task_id = %task_id,
            task_no = %task_no,
            owner_id = %owner_id,
            members,
            due_date = ?due_date,
            trace = ?trace,
            "任务节点已生成工作任务"
        );
    }
    Ok(outcome)
}

/// 渲染标题（**严格模式**：缺变量即失败，不静默留空）。
fn render_title(vars: &template_vars::TemplateVars, cfg: &TaskNodeConfig) -> std::result::Result<String, ApiError> {
    let title = render::render_paths(&cfg.title_template, &vars.dotted, ContentFormat::Text)?;
    let chars = title.chars().count();
    if chars > node_config::MAX_TITLE_TEMPLATE_CHARS {
        return Err(ApiError::new(
            400,
            error_code::FLOW_PREVIEW_INVALID,
            format!(
                "渲染后标题 {chars} 字符，超过 {}（biz_task.title 上限）",
                node_config::MAX_TITLE_TEMPLATE_CHARS
            ),
        ));
    }
    Ok(title)
}

/// 渲染描述（长度上限 8000 字符）。
fn render_description(vars: &template_vars::TemplateVars, template: &str) -> std::result::Result<String, ApiError> {
    let text = render::render_paths(template, &vars.dotted, ContentFormat::Text)?;
    let chars = text.chars().count();
    if chars > node_config::MAX_DESCRIPTION_TEMPLATE_CHARS {
        return Err(ApiError::new(
            400,
            error_code::FLOW_PREVIEW_INVALID,
            format!(
                "渲染后描述 {chars} 字符，超过 {}",
                node_config::MAX_DESCRIPTION_TEMPLATE_CHARS
            ),
        ));
    }
    Ok(text)
}

/// 算截止日（`config.task.due`；`form_mapping.due_date` 优先级更高）。
fn resolve_due_date(instance: &model::InstanceRow, cfg: &TaskNodeConfig) -> Option<NaiveDate> {
    // 表单映射优先：「表单里填了就按表单的」（设计 §2.3 form_mapping 的说明）。
    if let Some(path) = cfg.form_mapping.get("due_date")
        && let Some(raw) = form_value(&instance.form_data, path)
    {
        match parse_date_value(&raw) {
            Some(date) => return Some(date),
            None => tracing::warn!(
                path = %path,
                value = %raw,
                "form_mapping.due_date 指向的表单值不是日期，回退到 config.task.due"
            ),
        }
    }
    let due = cfg.due.as_ref()?;
    Some(compute_due(due, &instance.form_data, Utc::now()))
}

/// 按 `due` 配置算日期（纯函数，单测友好）。
///
/// - `RELATIVE`：从**节点激活时刻**（CST 日期）起算，`WORKDAY` 跳过周六周日；
/// - `ABSOLUTE`：配置里的固定日期；已过期就落到今天并记 WARN（设计 §8.4，不报错）；
/// - `FIELD`：从表单取日期；取不到 → 今天并记 WARN。
///
/// `form_data` 是发起时冻结的表单（一层 KV），`FIELD` 模式与 `form_mapping` 都从它取值。
pub fn compute_due(due: &DueConfig, form_data: &Json, now: chrono::DateTime<Utc>) -> NaiveDate {
    let today = cst_today(now);
    match due.mode {
        DueMode::Absolute => {
            let date = due.date.as_deref().and_then(parse_iso_date).unwrap_or(today);
            if date < today {
                tracing::warn!(
                    date = %date,
                    "config.task.due 的固定日期已过期，运行期落到当天（设计 §8.4）"
                );
                return today;
            }
            date
        }
        DueMode::Field => {
            let Some(path) = due.field.as_deref() else {
                return today;
            };
            match form_value(form_data, path).and_then(|raw| parse_date_value(&raw)) {
                Some(date) => date.max(today),
                None => {
                    tracing::warn!(
                        path = %path,
                        "config.task.due 取表单字段失败（缺失或不是日期），落到当天"
                    );
                    today
                }
            }
        }
        DueMode::Relative => {
            let amount = due.amount.unwrap_or(1);
            match due.unit.unwrap_or(DueUnit::Day) {
                DueUnit::Hour => {
                    let shifted = now + Duration::hours(amount);
                    cst_today(shifted)
                }
                DueUnit::Day => today + Duration::days(amount),
                DueUnit::Workday => add_workdays(today, amount),
            }
        }
    }
}

/// 加 N 个工作日（跳过周六 / 周日；本仓库暂无节假日表，见设计 §8.4）。
pub fn add_workdays(start: NaiveDate, amount: i64) -> NaiveDate {
    let mut date = start;
    let mut remaining = amount.max(0);
    while remaining > 0 {
        date += Duration::days(1);
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            remaining -= 1;
        }
    }
    date
}

/// 优先级（`form_mapping.priority` 命中且合法就用它，否则用配置值）。
fn resolve_priority(vars: &Json, cfg: &TaskNodeConfig) -> String {
    let Some(path) = cfg.form_mapping.get("priority") else {
        return cfg.priority.clone();
    };
    let Some(raw) = vars
        .get(path)
        .and_then(Json::as_str)
        .map(str::trim)
        .map(str::to_ascii_uppercase)
    else {
        return cfg.priority.clone();
    };
    if node_config::PRIORITIES.contains(&raw.as_str()) {
        raw
    } else {
        tracing::warn!(
            path = %path,
            value = %raw,
            "form_mapping.priority 指向的表单值不是 P0–P3，回退到配置值"
        );
        cfg.priority.clone()
    }
}

/// 预估工时（`form_mapping.estimate_hours`，仅接受 0 < x ≤ 9999.9）。
fn resolve_estimate_hours(vars: &Json, cfg: &TaskNodeConfig) -> Option<f64> {
    let path = cfg.form_mapping.get("estimate_hours")?;
    let value = vars.get(path)?;
    let number = match value {
        Json::Number(number) => number.as_f64(),
        Json::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    };
    match number {
        Some(hours) if (0.0..=9999.9).contains(&hours) => Some(hours),
        Some(hours) => {
            tracing::warn!(path = %path, hours, "form_mapping.estimate_hours 超出 0..9999.9，忽略");
            None
        }
        None => {
            tracing::warn!(path = %path, "form_mapping.estimate_hours 不是数字，忽略");
            None
        }
    }
}

/// 取一层嵌套 `form_data` 里的字段值（`form.x` → `form_data["x"]`）。
fn form_value(form_data: &Json, path: &str) -> Option<String> {
    let leaf = path.trim().strip_prefix("form.")?;
    let value = form_data.get(leaf)?;
    match value {
        Json::Null => None,
        Json::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// `yyyy-MM-dd` 解析。
fn parse_iso_date(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()
}

/// 日期值解析（字符串 `yyyy-MM-dd`，或 RFC3339 前缀 10 位）。
fn parse_date_value(raw: &str) -> Option<NaiveDate> {
    let trimmed = raw.trim();
    parse_iso_date(trimmed).or_else(|| {
        trimmed
            .get(..10)
            .and_then(|prefix| NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
    })
}

/// CST 的「今天」。
pub fn cst_today(now: chrono::DateTime<Utc>) -> NaiveDate {
    (now + Duration::hours(CST_OFFSET_HOURS)).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(
        mode: DueMode,
        amount: Option<i64>,
        unit: Option<DueUnit>,
        date: Option<&str>,
        field: Option<&str>,
    ) -> DueConfig {
        DueConfig {
            mode,
            amount,
            unit,
            date: date.map(str::to_string),
            field: field.map(str::to_string),
        }
    }

    fn instance(form: Json) -> model::InstanceRow {
        model::InstanceRow {
            id: Uuid::now_v7(),
            instance_no: "FL-1".to_string(),
            def_id: Uuid::now_v7(),
            version_id: Uuid::now_v7(),
            version_no: 1,
            flow_code: "t".to_string(),
            flow_name: "t".to_string(),
            biz_type: "PURCHASE_ORDER".to_string(),
            biz_id: None,
            biz_no: Some("PO1".to_string()),
            title: "t".to_string(),
            status: "PENDING".to_string(),
            initiator_id: Uuid::now_v7(),
            initiator_name: None,
            initiator_dept_id: None,
            form_data: form,
            result_comment: None,
            auto_passed_nodes: 0,
            round_total: 0,
            prev_instance_id: None,
            revision: 1,
            started_at: Utc::now(),
            ended_at: None,
            due_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// 2026-11-01 是周日（设计 §3.3 的示例基准）。
    fn sunday() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-01T09:00:00+08:00")
            .expect("固定时刻")
            .with_timezone(&Utc)
    }

    /// 工作日加算跳过周六周日：从周日(11-01) 起 3 个工作日 = 11-04（周三）。
    #[test]
    fn workday_due_skips_weekend() {
        let start = NaiveDate::from_ymd_opt(2026, 11, 1).expect("合法日期");
        assert_eq!(
            add_workdays(start, 3),
            NaiveDate::from_ymd_opt(2026, 11, 4).expect("合法日期")
        );
        // 周五 + 1 个工作日 = 下周一
        let friday = NaiveDate::from_ymd_opt(2026, 10, 30).expect("合法日期");
        assert_eq!(
            add_workdays(friday, 1),
            NaiveDate::from_ymd_opt(2026, 11, 2).expect("合法日期")
        );
    }

    /// RELATIVE 三种单位：HOUR / DAY / WORKDAY。
    #[test]
    fn relative_due_covers_three_units() {
        let row = instance(json!({}));
        let hour = compute_due(
            &due(DueMode::Relative, Some(6), Some(DueUnit::Hour), None, None),
            &row.form_data,
            sunday(),
        );
        // 09:00 CST + 6h = 15:00 CST，仍是同一天
        assert_eq!(hour, NaiveDate::from_ymd_opt(2026, 11, 1).expect("合法日期"));
        let day = compute_due(
            &due(DueMode::Relative, Some(3), Some(DueUnit::Day), None, None),
            &row.form_data,
            sunday(),
        );
        assert_eq!(day, NaiveDate::from_ymd_opt(2026, 11, 4).expect("合法日期"));
        let workday = compute_due(
            &due(DueMode::Relative, Some(3), Some(DueUnit::Workday), None, None),
            &row.form_data,
            sunday(),
        );
        assert_eq!(workday, NaiveDate::from_ymd_opt(2026, 11, 4).expect("合法日期"));
    }

    /// ABSOLUTE 已过期 → 落到当天（不报错，只 WARN，设计 §8.4）。
    #[test]
    fn absolute_due_falls_back_to_today_when_expired() {
        let row = instance(json!({}));
        let date = compute_due(
            &due(DueMode::Absolute, None, None, Some("2020-01-01"), None),
            &row.form_data,
            sunday(),
        );
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 11, 1).expect("合法日期"));
    }

    /// FIELD 从表单取日期；取不到 → 当天。
    #[test]
    fn field_due_reads_form_value() {
        let row = instance(json!({"delivery_date": "2026-11-15"}));
        let date = compute_due(
            &due(DueMode::Field, None, None, None, Some("form.delivery_date")),
            &row.form_data,
            sunday(),
        );
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 11, 15).expect("合法日期"));

        let empty = instance(json!({}));
        let fallback = compute_due(
            &due(DueMode::Field, None, None, None, Some("form.delivery_date")),
            &empty.form_data,
            sunday(),
        );
        assert_eq!(fallback, NaiveDate::from_ymd_opt(2026, 11, 1).expect("合法日期"));
    }
}
