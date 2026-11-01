//! `NOTIFY_NODE` 引擎分支（P-FLOW-2 · 设计 §2.6 / §2.7 R19）。
//!
//! # 一句话
//!
//! **通知是「尽力而为」的副作用，不是流程的必经环节。**
//!
//! 这个界限必须在代码里写死，否则两个方向都会出事：
//!
//! | 故障 | 判据 | 处置 | 为什么 |
//! |------|------|------|--------|
//! | **渲染失败** | 内联模板语法错 / 缺变量 / 超长 —— 是**配置错误** | `400 + 65036`，整笔回滚 | 让人去改配置；放过去则每次走到这个节点都失败 |
//! | **投递失败** | `notice::dispatch` 报错（模板被停用 / DB 抖动 / 通道挂了） | 只 `tracing::warn!`，流程继续 | 邮件服务挂了不该让整个审批卡死 |
//!
//! 分界点落在**事务边界**上：本模块只在引擎事务内做「解析收件人 + 渲染 + 组装
//! `NoticeDraft`」，真正的 `dispatch` 由 `engine::flush_drafts` 在**事务提交之后**调用
//! （与既有 `engine::flush_effects` 完全同款）。因此：
//!
//! - 渲染失败发生在事务内 → 引擎拿到 `Step::Fail` → 整笔回滚 → 用户改配置重试；
//! - 投递失败发生在提交后 → 业务早已落库 → 只记 WARN，用户刷新看到的是「流程已推进」。
//!
//! # 三种时机（`config.notify.when`）
//!
//! | 值 | 引擎行为 |
//! |----|----------|
//! | `ON_ENTER` | 渲染 + 产生 effect → 节点 `APPROVED` → 驱动下游 |
//! | `ON_LEAVE` | 节点 `APPROVED` → 渲染 + 产生 effect → 驱动下游 |
//! | `ON_TIMEOUT` | 节点保持 `ACTIVE`，**不发**；`timeout_hours` 后由 `engine::fire_notify_node` 发送并放行 |
//!
//! **`ON_ENTER` 与 `ON_LEAVE` 的投递时刻相同**（都在提交后），差别只有「通知时点相对
//! 节点通过的先后」。之所以保留两个值：设计文档 §2.6 定义了它们、前端下拉框已经这么画，
//! 删掉会变成「前端有选项、后端不认」。这里不假装它们有阻塞语义差异 —— 本节点**永不阻塞**
//! （唯一例外是 `ON_TIMEOUT` 的等待期，那是配置出来的，不是节点的固有属性）。
//!
//! # 收件人：复用 `assignee::resolve`，不写第二套指派规则
//!
//! 每个 `target` 被翻译成一个「临时 [`SnapshotNode`]」再交给 `assignee::resolve`：
//! `assignee_type` 与 `config.notify.targets[].type` **逐字相同**（`USER` / `ROLE` /
//! `INITIATOR` / `INITIATOR_SUPERIOR` / `DEPT_LEADER` / `TASK_OWNER` / `TASK_CREATOR` /
//! `TASK_ASSIGNER` / `FORM_VAR` / `ADMIN`），所以那不是「翻译」，是直接透传。
//! 三处刻意的策略覆盖（写在 [`target_spec`] 里并注明理由）：
//!
//! 1. `self_approval = ALLOW` —— 通知必须能发给自己（「通知发起人」是最常见的配置）；
//! 2. `empty_assignee = SKIP` —— 某个来源解析出 0 人只是少一个收件人，不是 65018；
//! 3. `allow_robot = true` —— 机器人账号也能收通知（与「机器人不能审批」是两回事）。
//!
//! # 落库标记（`sys_notice`）
//!
//! 卡面写 `source_type = 'FLOW_NOTIFY' / source_id = 实例 id / biz_no = 实例单号`。
//! 查完 `sys_notice` 的 DDL（V0043）**没有这三列**，等价字段是：
//!
//! | 卡面 | 实际落库 | 说明 |
//! |------|----------|------|
//! | `source_type` | `biz_type = 'FLOW_NOTIFY'` | 与审批通知 `biz_type='workflow'` 区分开，通知中心可按它筛「流程通知节点发的」 |
//! | `source_id` | `biz_id = 实例 id` | 与实例强关联，实例终止后通知仍在（审计，L6） |
//! | `biz_no` | `template_vars.instanceNo`（+ `link_url` 指向实例） | `sys_notice` 没有单号列，单号进模板变量与跳转链接 |
//!
//! 另外 `template_vars.nodeKey / nodeName` 是**故意**写进去的：实例详情要显示
//! 「本节点已发通知 N 人」，靠 `GROUP BY template_vars->>'nodeKey'` 一次聚合即可，不加列。

use std::collections::BTreeSet;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::assignee::{self, InstanceCtx, Resolution};
use crate::business::flow::graph::{EvalContext, SnapshotAssignee, SnapshotNode};
use crate::business::flow::model;
use crate::business::flow::node_config::{
    self, NotifyNodeConfig, NotifyTemplate, NotifyWhen, SkipIfEmpty,
};
use crate::business::flow::template_vars;
use crate::business::notice::dto::{ContentFormat, NoticeLevel, NoticeType};
use crate::business::notice::{NoticeDraft, NoticeTarget};

/// 写进 `sys_notice.biz_type` 的标记（卡面的 `source_type = 'FLOW_NOTIFY'`）。
pub const BIZ_TYPE: &str = "FLOW_NOTIFY";

/// 跳转链接前缀（与 `flow::notify::LINK_PREFIX` 同一个前端路由）。
pub const LINK_PREFIX: &str = "/flow/instance/";

/// 发送人展示名。
const SENDER_NAME: &str = "工作流引擎";

/// 一次通知节点的组装结果（**不含投递**：投递在事务提交后由 `engine::flush_drafts` 做）。
#[derive(Debug)]
pub enum NotifyOutcome {
    /// 组装好了：`draft` 交给引擎收集，提交后统一投递。
    ///
    /// `draft` 用 `Box`：`NoticeDraft` 有 ~300 字节（模板 / 变量 / 目标 / 幂等键），
    /// 直接当枚举变体会让 `clippy::large_enum_variant` 报错，也让 `Failed` 这条
    /// 常见路径白扛这份体积。
    Ready {
        /// 完整草稿（模板 / 变量 / 收件人 / 幂等键都已填好）。
        draft: Box<NoticeDraft>,
        /// 收件人数（去重后）。
        recipients: usize,
        /// 解析痕迹（`INITIATOR 命中 1 行` 这类），排障用。
        trace: Vec<String>,
    },
    /// 一个收件人都没解析出来，且 `skip_if_empty != ERROR` → 不投递，流程继续。
    Nobody {
        /// 解析痕迹。
        trace: Vec<String>,
        /// 生效的策略（`PASS` 静默 / `WARN` 打日志）。
        policy: SkipIfEmpty,
    },
    /// 配置错误（渲染失败 / `skip_if_empty=ERROR` 且无人可发）→ 引擎整笔回滚。
    Failed(ApiError),
}

impl NotifyOutcome {
    /// 业务拒绝的错误（`Failed` 之外恒 `None`）。
    pub fn error(self) -> Option<ApiError> {
        match self {
            NotifyOutcome::Failed(error) => Some(error),
            _ => None,
        }
    }
}

/// 组装一条通知节点的副作用（**必须在引擎事务内调用**）。
///
/// # 参数
///
/// - `ex`：引擎事务；
/// - `instance`：流程实例行（模板变量、收件人上下文都从它取）；
/// - `spec`：快照节点（`node_type = NOTIFY_NODE`）；
/// - `round`：节点轮次（幂等键 + 模板变量口径）；
/// - `cfg`：已解析的 `config.notify`；
/// - `ctx` / `eval`：与审批节点**同一份**指派上下文与求值上下文（不造第二套规则）。
///
/// # 异常情况
///
/// - 数据库异常 → [`DbError`]（引擎整笔回滚）；
/// - 配置错误（渲染失败）→ [`NotifyOutcome::Failed`]（`400 + 65036`，引擎转 `Step::Fail`）。
pub async fn build(
    ex: &impl SqlExecutor,
    instance: &model::InstanceRow,
    spec: &SnapshotNode,
    round: i32,
    cfg: &NotifyNodeConfig,
    ctx: &InstanceCtx,
    eval: &EvalContext,
) -> DbResult<NotifyOutcome> {
    let mut trace: Vec<String> = Vec::new();

    // ---------- ① 解析收件人 ----------
    let mut users: BTreeSet<Uuid> = BTreeSet::new();
    for (index, target) in cfg.targets.iter().enumerate() {
        let prepared = match prepare_target(ex, spec, target, &mut trace).await? {
            Ok(prepared) => prepared,
            Err(error) => return Ok(NotifyOutcome::Failed(error)),
        };
        let Resolution {
            finalized,
            trace: inner,
        } = assignee::resolve(ex, &prepared, ctx, eval).await?;
        // 收件人解析**不允许**把节点判成「业务失败」：它只是少一个收件人。
        // `finalize()` 在 `empty_assignee=SKIP` 下不会产生 error（我们强制 SKIP），
        // 真要出 error 也只可能是表达式结构坏 —— 那是配置错误，如实上报。
        if let Some(error) = finalized.error {
            return Ok(NotifyOutcome::Failed(error));
        }
        let count = finalized.approvers.len();
        users.extend(finalized.approvers.into_iter().map(|item| item.user_id));
        trace.push(format!(
            "targets[{index}]={} → {count} 人（{}）",
            target.target_type.as_str(),
            inner.join(" / ")
        ));
    }

    // ---------- ② 追加「上一审批人」（可选） ----------
    if cfg.prev_approver
        && let Some((name, id)) = template_vars::prev_approver(ex, instance.id, round).await?
    {
        users.insert(id);
        trace.push(format!("prev_approver 追加 {name}"));
    }

    let recipients: Vec<Uuid> = users.into_iter().collect();
    if recipients.is_empty() {
        if cfg.skip_if_empty == SkipIfEmpty::Error {
            return Ok(NotifyOutcome::Failed(node_config::notify_template_invalid(
                format!(
                    "通知节点 {}（{}）没有解析出任何收件人，且 skip_if_empty=ERROR：{}",
                    spec.key,
                    spec.name,
                    trace.join("; ")
                ),
            )));
        }
        return Ok(NotifyOutcome::Nobody {
            trace,
            policy: cfg.skip_if_empty,
        });
    }

    // ---------- ③ 模板变量（点号键给 render_paths，扁平键给 dispatch） ----------
    let vars = template_vars::build(ex, instance, spec, round, false).await?;
    for warning in &vars.warnings {
        trace.push(warning.clone());
    }

    // ---------- ④ 组装草稿 ----------
    let dotted = vars.dotted;
    let flat = with_node_marks(&vars.flat, spec);
    let mut draft = match &cfg.template {
        NotifyTemplate::Inline {
            title,
            content,
            content_format,
        } => {
            // 解析器已经保证 content_format 只能是 TEXT / HTML，这里的 `unwrap_or` 只是
            // 防「库被直接改过」—— 真出错也退化成 TEXT 而不是 500。
            let format = ContentFormat::parse(content_format).unwrap_or(ContentFormat::Text);
            // ★ 渲染失败 = 配置错误 → 400（不是运行时故障）。这一条必须硬失败：
            //   放过去会让「每次走到这个节点都发不出去」，比 500 更难发现。
            let rendered_title = match render_or_fail(spec, "inline.title", title, &dotted, format)
            {
                Ok(text) => text,
                Err(error) => return Ok(NotifyOutcome::Failed(error)),
            };
            let rendered_content =
                match render_or_fail(spec, "inline.content", content, &dotted, format) {
                    Ok(text) => text,
                    Err(error) => return Ok(NotifyOutcome::Failed(error)),
                };
            let mut draft = NoticeDraft::text(
                NoticeType::Workflow,
                notice_level(cfg),
                rendered_title,
                rendered_content,
                NoticeTarget::User(recipients.clone()),
            );
            draft.content_format = format;
            draft
        }
        NotifyTemplate::Code { template_code } => {
            let mut draft = NoticeDraft::text(
                NoticeType::Workflow,
                notice_level(cfg),
                String::new(),
                String::new(),
                NoticeTarget::User(recipients.clone()),
            );
            draft.template_code = Some(template_code.clone());
            draft
        }
    };
    // `render()`（template_code 路径）只认无点号名 → 用 `flat`；内联路径同理只为留痕。
    // `with_flow_aliases` 再补一组 camelCase（`{{flowName}}` / `{{nodeName}}` …），
    // 让 `template_code` 能引用 V0125 种下的 4 个 `FLOW_*` 模板。
    draft.template_vars = with_flow_aliases(flat, instance, spec);
    draft.link_url = Some(format!("{LINK_PREFIX}{}", instance.id));
    draft.biz_type = Some(BIZ_TYPE.to_string());
    draft.biz_id = Some(instance.id.to_string());
    draft.idempotency_key = Some(idempotency_key(instance.id, &spec.key, round, cfg.when));
    draft.sender_name = Some(SENDER_NAME.to_string());
    draft.channel_mask = cfg.channels;
    draft.force = cfg.force;

    Ok(NotifyOutcome::Ready {
        draft: Box::new(draft),
        recipients: recipients.len(),
        trace,
    })
}

/// 渲染一段内联模板；失败统一抬成 `400 + 65036`（含节点 key，便于定位）。
///
/// # 为什么返回 `ApiError` 而不是 `NotifyOutcome`
///
/// `NotifyOutcome::Ready` 带 `Box<NoticeDraft>` 之后枚举本身已经不大了，但把
/// `NotifyOutcome` 放在 `Err` 位仍会被 `clippy::result_large_err` 拦；
/// 而「渲染失败」本质就是一个 `ApiError`，调用方 `return Ok(Failed(error))` 一行就够。
fn render_or_fail(
    spec: &SnapshotNode,
    label: &str,
    template: &str,
    vars: &serde_json::Value,
    format: ContentFormat,
) -> std::result::Result<String, ApiError> {
    // 运行期用 `Text` / `Html` 都要过 `render_paths`；HTML 时值会被转义（`escape_html`），
    // 与 `notice::dispatch` 的模板渲染口径一致。
    crate::business::notice::render::render_paths(template, vars, format).map_err(|error| {
        node_config::notify_template_invalid(format!(
            "通知节点 {}（{}）的 {label} 渲染失败：{}（这是配置错误，请改模板或补变量）",
            spec.key, spec.name, error.msg
        ))
    })
}

/// 把 `nodeKey` / `nodeName` 塞进模板变量（实例详情按它聚合「已发通知 N 人」）。
fn with_node_marks(vars: &serde_json::Value, spec: &SnapshotNode) -> serde_json::Value {
    let mut object = vars.as_object().cloned().unwrap_or_default();
    object.insert(
        "nodeKey".to_string(),
        serde_json::Value::String(spec.key.clone()),
    );
    object.insert(
        "nodeName".to_string(),
        serde_json::Value::String(spec.name.clone()),
    );
    serde_json::Value::Object(object)
}

/// 补一组 **camelCase 别名**，让 `template_code` 能直接引用库里现成的 `FLOW_*` 模板。
///
/// # 为什么必须要这一组
///
/// `sys_notice_template` 的 `{{var}}` 由 `notice::render::render()` 解析，变量名文法被锁在
/// 「无点号」，而 V0125 种下的 4 个审批模板（`FLOW_TODO` / `FLOW_RESULT` / `FLOW_CC` /
/// `FLOW_URGE`）用的是 **camelCase**：`{{flowName}}` / `{{instanceNo}}` / `{{nodeName}}` /
/// `{{initiatorName}}` / `{{title}}` / `{{resultText}}` / `{{comment}}` / `{{urgeCount}}`。
/// 而 `template_vars::build` 的扁平键是 `snake_case`（`instance_title` / `initiator_name` …）。
///
/// 两者都要支持：引用库模板时用 camelCase，内联模板时用点号路径（`{{instance.title}}`）。
/// 一个变量表里同时放两套名字，比「让用户记两套规则」和一个转换表都便宜，
/// 也和 `template_vars` 头注释里「一个构造器产出两份键」的设计一脉相承。
///
/// **八个键恒定存在**（不用的给空串）：`render()` 对缺失变量是 422 硬失败，
/// 「这个模板用不到所以不传」会让模板改一个字就整条通知发不出去（与 `notify::FlowNotice`
/// 的 `template_vars` 同一条纪律）。
fn with_flow_aliases(
    vars: serde_json::Value,
    instance: &model::InstanceRow,
    spec: &SnapshotNode,
) -> serde_json::Value {
    let mut object = vars.as_object().cloned().unwrap_or_default();
    let mut put = |key: &str, value: String| {
        object.insert(key.to_string(), serde_json::Value::String(value));
    };
    put("flowName", instance.flow_name.clone());
    put("instanceNo", instance.instance_no.clone());
    put("nodeName", spec.name.clone());
    put(
        "initiatorName",
        instance.initiator_name.clone().unwrap_or_default(),
    );
    put("title", instance.title.clone());
    put("resultText", String::new());
    put("comment", String::new());
    put("urgeCount", "1".to_string());
    serde_json::Value::Object(object)
}

/// 配置里的级别 → `NoticeLevel`。
fn notice_level(cfg: &NotifyNodeConfig) -> NoticeLevel {
    match cfg.level.as_db_str() {
        "WARN" => NoticeLevel::Warn,
        "ERROR" => NoticeLevel::Error,
        _ => NoticeLevel::Info,
    }
}

/// 幂等键：`FLOW:NOTIFY:<E|L|T>:<实例>:<节点>:<轮次>`。
///
/// # 为什么带时机段
///
/// 同一节点改成 `ON_ENTER → ON_LEAVE` 后重新发布，旧实例（冻结旧快照）与新实例互不影响；
/// 但**同一实例**里如果两套时机都触发（只有 ON_TIMEOUT 会，其它两个在进入时就定了），
/// 不带时机段会让第二次静默命中幂等键。
///
/// # 三个「短写」不是审美，是长度硬约束
///
/// `sys_notice.idempotency_key` 是 `varchar(128)`，超了直接 500。最坏情形
/// （节点 key 用满 64 字符 + 轮次 `i32::MAX`）必须装得下，所以：
///
/// | 位置 | 取值 | 字数 |
/// |------|------|------|
/// | 前缀 | `FLOW:NOTIFY:` | 12 |
/// | 时机 | `E` / `L` / `T` | 1 |
/// | 实例 | `Uuid::simple()`（无连字符 32 位） | 32 |
/// | 节点 | 上限 64（`^[A-Za-z][A-Za-z0-9_]{0,63}$`） | ≤64 |
/// | 轮次 | `MAX_ROUND = 50`；理论最坏 11 | ≤11 |
/// | 合计 | | **≤ 121** |
///
/// 实例 id 用无连字符形态只影响这一列；`sys_notice.biz_id` 里仍存完整的 `hyphenated`，
/// 按实例 grep 审计行不受影响。
pub fn idempotency_key(instance_id: Uuid, node_key: &str, round: i32, when: NotifyWhen) -> String {
    format!(
        "FLOW:NOTIFY:{}:{}:{}:{}",
        when.idempotency_segment(),
        instance_id.simple(),
        node_key,
        round.max(1)
    )
}

/// 把一个 `target` 翻译成 `assignee::resolve` 认识的 [`SnapshotNode`]。
///
/// # 三处策略覆盖（每一条都有业务理由，别照抄成默认值）
///
/// | 字段 | 值 | 理由 |
/// |------|----|------|
/// | `self_approval` | `ALLOW` | 「通知发起人」是最常见配置；沿用审批的 `SKIP` 会让它永远 0 人 |
/// | `empty_assignee` | `SKIP` | 某个来源解析出 0 人只是少一个收件人，不该 65018 |
/// | `allow_robot` | `true` | 机器人账号也要能收通知 |
///
/// `config` 被清空：通知节点自己的 `config.notify` 不能被 `resolve()` 当成
/// `EXPRESSION` 的 `config.expression` 去解析（那会 501）。`EXPRESSION` 也不在
/// `targets[].type` 的 10 值枚举里（设计 §2.6），所以这是防御性清零。
async fn prepare_target(
    ex: &impl SqlExecutor,
    spec: &SnapshotNode,
    target: &node_config::NotifyTarget,
    trace: &mut Vec<String>,
) -> DbResult<std::result::Result<SnapshotNode, ApiError>> {
    let mut prepared = spec.clone();
    prepared.assignee_type = Some(target.target_type.assignee_type().to_string());
    prepared.self_approval = "ALLOW".to_string();
    prepared.empty_assignee = "SKIP".to_string();
    prepared.allow_robot = true;
    prepared.superior_levels = target.levels;
    prepared.form_var_key = target.form_var_key.clone();
    prepared.config = serde_json::Value::Object(serde_json::Map::new());
    prepared.assignees = Vec::new();

    let mut order = 0i32;
    let push = |prepared: &mut SnapshotNode, kind: &str, ref_id: String, order: &mut i32| {
        *order += 1;
        prepared.assignees.push(SnapshotAssignee {
            assignee_type: kind.to_string(),
            ref_id,
            sort_order: *order,
        });
    };

    if !target.user_ids.is_empty() {
        for id in &target.user_ids {
            push(&mut prepared, "USER", id.clone(), &mut order);
        }
    }
    if !target.dept_ids.is_empty() {
        for id in &target.dept_ids {
            push(&mut prepared, "DEPT_LEADER", id.clone(), &mut order);
        }
    }
    if !target.role_codes.is_empty() {
        // 角色编码 → 角色 id：复用 `assignee::load_role_ids_by_code`（与表达式审批人同一个
        // code→id 解析，不另写一份）。查不到不算错：这条 target 解析出 0 人。
        let ids = assignee::load_role_ids_by_code(ex, &target.role_codes, trace).await?;
        if ids.is_empty() {
            trace.push(format!(
                "target ROLE 的编码 {:?} 一个都没命中（角色被删 / 改名？）",
                target.role_codes
            ));
        }
        for id in ids {
            push(&mut prepared, "ROLE", id.to_string(), &mut order);
        }
    }

    Ok(Ok(prepared))
}

// ==================== ON_TIMEOUT：定时扫描 ====================
//
// 复用 `flow_timeout_scan` 的**同一个 cron**（每 10 分钟），不新增定时任务：
// 两者都是「到点了该做点什么」的扫描，拆两个 cron 只会让运维多记一条。

/// 单批扫描上限（通知节点比待办少几个数量级，200 足够）。
pub const NOTIFY_SCAN_BATCH: i64 = 200;

/// 待发通知的通知节点（`ON_TIMEOUT` 且已过停留时长）。
///
/// `CROSS JOIN LATERAL` 从发布快照里取 `timeout_hours` 与 `when`：
/// 快照是**不可变**的，与实例 `version_id` 冻结绑定（设计 D2），不会读到
/// 「定义行被改了但实例还在跑旧图」的脏配置。
const SQL_DUE: &str = "SELECT n.instance_id AS instance_id, n.node_key AS node_key, \
        n.round AS round \
   FROM biz_flow_instance_node n \
   JOIN biz_flow_instance i ON i.id = n.instance_id \
   JOIN biz_flow_def_version v ON v.id = i.version_id \
   CROSS JOIN LATERAL ( \
        SELECT (elem -> 'config' -> 'notify' ->> 'when') AS when_kind, \
               (elem ->> 'timeout_hours')::int AS hours \
          FROM jsonb_array_elements(COALESCE(v.graph_snapshot -> 'nodes', '[]'::jsonb)) elem \
         WHERE elem ->> 'key' = n.node_key \
         LIMIT 1 \
   ) cfg \
  WHERE n.status = 'ACTIVE' \
    AND n.node_type = 'NOTIFY_NODE' \
    AND i.status = 'PENDING' \
    AND i.deleted_at IS NULL \
    AND cfg.when_kind = 'ON_TIMEOUT' \
    AND cfg.hours IS NOT NULL AND cfg.hours > 0 \
    AND COALESCE(n.activated_at, n.created_at) + make_interval(hours => cfg.hours) <= now() \
  ORDER BY n.activated_at \
  LIMIT ?";

/// 一个到点的通知节点。
#[derive(Debug, Clone)]
pub struct DueNotify {
    /// 实例。
    pub instance_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
}

/// 查一批到点的通知节点（`SCOPE-EXEMPT`：定时任务没有请求者身份）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
///
/// SCOPE-EXEMPT: 后台定时扫描，按 `activated_at + timeout_hours <= now()` 全局取数；
/// 注入数据范围会让别人的定时通知永远不发。
pub async fn due_nodes() -> DbResult<Vec<DueNotify>> {
    let rows = db()
        .find_with_params(SQL_DUE, vec![Value::from(NOTIFY_SCAN_BATCH)])
        .await?;
    let mut out: Vec<DueNotify> = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(DueNotify {
            instance_id: row.get_typed("instance_id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
        });
    }
    Ok(out)
}

/// 执行一轮「到点通知」扫描：逐条调 `engine::fire_notify_node` 并投递副作用。
///
/// # 返回值
///
/// 成功放行（并投递）的条数。失败条目只记 WARN 并继续处理后面的
/// —— 一条卡住不能把整批拖死（与 `timeout::run_once` 同款纪律）。
///
/// # 异常情况
///
/// 只有**扫描 SQL** 失败才返回错误（`String`，调用方只记日志）；
/// 单条的失败不影响其它条目。
pub async fn run_due_once() -> std::result::Result<usize, String> {
    let due = due_nodes()
        .await
        .map_err(|error| format!("扫描到点的通知节点失败：{error}"))?;
    let mut fired = 0usize;
    for item in due {
        let instance_id = item.instance_id;
        let node_key = item.node_key.clone();
        let round = item.round;
        let key_for_tx = node_key.clone();
        let outcome = db()
            .transaction(move |tx| async move {
                crate::business::flow::engine::fire_notify_node(
                    &tx,
                    instance_id,
                    &key_for_tx,
                    round,
                )
                .await
            })
            .await;
        let mut outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(
                    %instance_id,
                    node_key,
                    error = %error,
                    "通知节点（ON_TIMEOUT）处理失败"
                );
                continue;
            }
        };
        if let Some(error) = outcome.rejected.take() {
            // 幂等命中 / 实例已结束都走这里：不是缺陷，记 debug 就够
            tracing::debug!(
                %instance_id,
                node_key,
                code = error.code,
                msg = %error.msg,
                "通知节点（ON_TIMEOUT）未发送"
            );
            continue;
        }
        // 副作用锁外发送（与 handler 的 drain 同口径）
        crate::business::flow::engine::flush_effects(outcome.effects.take()).await;
        crate::business::flow::engine::flush_drafts(outcome.effects.take_drafts()).await;
        fired += 1;
        tracing::info!(
            %instance_id,
            node_key,
            round,
            "通知节点（ON_TIMEOUT）已发送并放行"
        );
    }
    Ok(fired)
}

/// 发布期检查：`template_code` 引用的模板必须存在且启用（R19 的 DB 部分）。
///
/// # 为什么不在 `FlowGraph::validate_with` 里
///
/// 那是**纯函数**（60 多条单测、编辑器「只校验不保存」都调它），拿不到数据库。
/// 需要在发布事务前查库的判据都收在 handler 里（与 `approve_biz_type` 同款，
/// 见 `handler::approve_biz_type_issues`）。
///
/// # 返回值
///
/// 命中的问题（`65036`，文案点名节点与模板编码）；全部存在 → 空数组。
pub async fn template_issues(
    graph: &crate::business::flow::graph::FlowGraph,
) -> DbResult<Vec<crate::business::flow::graph::Issue>> {
    use crate::business::flow::graph::{Issue, NodeType};
    use crate::shared::error_code;

    let mut issues: Vec<Issue> = Vec::new();
    for node in &graph.nodes {
        if node.node_type != NodeType::Notify {
            continue;
        }
        let Ok(config) = node_config::parse_notify_config(&node.config["notify"]) else {
            // 形状已经不合法（R19 报过），这里不重复报「模板不存在」
            continue;
        };
        let Some(code) = config.template.template_code() else {
            continue;
        };
        let found = crate::business::notice::model::find_enabled_template_by_code(code)
            .await
            .map_err(|error| DbError::InvalidValue(format!("查询通知模板 {code} 失败：{error}")))?;
        if found.is_none() {
            issues.push(
                Issue::new(
                    "R19",
                    error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
                    format!(
                        "通知节点 {}（{}）引用的通知模板 {code} 不存在或已停用（系统 → 通知模板里要先建好并启用）",
                        node.key, node.name
                    ),
                )
                .with_nodes(vec![node.key.clone()]),
            );
        }
    }
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::graph::NodeType;
    use serde_json::json;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn spec() -> SnapshotNode {
        SnapshotNode {
            key: "n_notify".to_string(),
            name: "通知发起人".to_string(),
            node_type: NodeType::Notify.as_str().to_string(),
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
            config: json!({}),
            assignees: Vec::new(),
        }
    }

    fn cfg(when: NotifyWhen) -> NotifyNodeConfig {
        node_config::parse_notify_config(&json!({
            "when": when.as_str(),
            "targets": [{"type": "INITIATOR"}],
            "inline": {"title": "{{instance.title}}", "content": "单号 {{instance.biz_no}}"}
        }))
        .expect("夹具配置必须合法")
    }

    /// 幂等键：同输入稳定、轮次 / 节点 / 时机变则变、长度不越界。
    #[test]
    fn idempotency_key_is_scoped_and_short() {
        let id = uuid(0x1234);
        let base = idempotency_key(id, "n_notify", 1, NotifyWhen::OnEnter);
        assert_eq!(
            base,
            idempotency_key(id, "n_notify", 1, NotifyWhen::OnEnter)
        );
        assert_ne!(
            base,
            idempotency_key(id, "n_notify", 2, NotifyWhen::OnEnter)
        );
        assert_ne!(base, idempotency_key(id, "n_other", 1, NotifyWhen::OnEnter));
        assert_ne!(
            base,
            idempotency_key(id, "n_notify", 1, NotifyWhen::OnLeave)
        );
        assert!(base.starts_with("FLOW:NOTIFY:E:"));
        // 最长情形：64 字符节点 key + 3 位轮次 → 必须 ≤ 128（`sys_notice.idempotency_key`）
        let longest = idempotency_key(id, &"n".repeat(64), i32::MAX, NotifyWhen::OnTimeout);
        assert!(
            longest.chars().count() <= 128,
            "幂等键 {} 个字符，超过 sys_notice.idempotency_key 的 128",
            longest.chars().count()
        );
        // 轮次下限：0 / 负数都按第 1 轮（与 `notify::FlowNotice::with_round` 同口径）
        assert_eq!(
            idempotency_key(id, "n_notify", 0, NotifyWhen::OnEnter),
            base,
            "轮次 0 必须归一到 1"
        );
    }

    /// 三种时机的幂等键互不相同（防止 `when` 改了之后新旧通知互相吞掉）。
    #[test]
    fn every_when_has_its_own_key_segment() {
        let id = uuid(7);
        let keys: Vec<String> = NotifyWhen::ALL
            .iter()
            .map(|when| idempotency_key(id, "n_notify", 1, *when))
            .collect();
        let unique: BTreeSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), 3, "三种时机必须产生三个不同的键：{keys:?}");
        assert!(keys[0].contains(":E:"));
        assert!(keys[1].contains(":L:"));
        assert!(keys[2].contains(":T:"));
    }

    /// 内联渲染失败 → `400 + 65036`（**配置错误，不是运行时故障**）。
    #[test]
    fn render_failure_maps_to_400() {
        let error = render_or_fail(
            &spec(),
            "inline.title",
            "{{instance.title",
            &json!({"instance.title": "x"}),
            ContentFormat::Text,
        )
        .expect_err("未闭合的占位符必须失败");
        assert_eq!(error.status, 400, "渲染失败必须是 400 而不是 500");
        assert_eq!(
            error.code,
            crate::shared::error_code::FLOW_NOTIFY_TEMPLATE_INVALID
        );
        assert!(
            error.msg.contains("n_notify"),
            "文案要点名节点：{}",
            error.msg
        );
    }

    /// 缺变量也是渲染失败（`render_paths` 的规则 2：不静默替换成空串）。
    #[test]
    fn missing_variable_is_a_render_failure() {
        let error = render_or_fail(
            &spec(),
            "inline.content",
            "单号 {{instance.biz_no}}",
            &json!({"instance.title": "x"}),
            ContentFormat::Text,
        )
        .expect_err("缺变量必须失败");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("instance.biz_no"), "{}", error.msg);
    }

    /// 内联渲染成功 + `nodeKey/nodeName` 被塞进模板变量（实例详情聚合靠它）。
    #[test]
    fn node_marks_are_added_to_vars() {
        let vars = with_node_marks(&json!({"instance_title": "采购申请"}), &spec());
        assert_eq!(vars["nodeKey"], json!("n_notify"));
        assert_eq!(vars["nodeName"], json!("通知发起人"));
        assert_eq!(vars["instance_title"], json!("采购申请"), "原有变量不能丢");
    }

    /// 内联渲染通过：点号变量按 `render_paths` 口径替换。
    #[test]
    fn inline_render_uses_dotted_paths() {
        let text = render_or_fail(
            &spec(),
            "inline.title",
            "{{instance.title}} 已到「{{node.name}}」",
            &json!({"instance.title": "采购申请", "node.name": "通知发起人"}),
            ContentFormat::Text,
        )
        .expect("变量齐全必须渲染成功");
        assert_eq!(text, "采购申请 已到「通知发起人」");
    }

    /// 三种时机的 `is_deferred`：只有 `ON_TIMEOUT` 挂起。
    #[test]
    fn deferred_only_for_timeout() {
        assert!(!cfg(NotifyWhen::OnEnter).when.is_deferred());
        assert!(!cfg(NotifyWhen::OnLeave).when.is_deferred());
        assert!(cfg(NotifyWhen::OnTimeout).when.is_deferred());
    }

    /// 级别映射：`URGENT` 落库 `ERROR`（`sys_notice.level` 的取值域里没有 URGENT）。
    #[test]
    fn urgent_level_maps_to_error() {
        let mut config = cfg(NotifyWhen::OnEnter);
        config.level = node_config::NotifyLevel::Urgent;
        assert_eq!(notice_level(&config).as_str(), "ERROR");
        assert_eq!(node_config::NotifyLevel::Urgent.as_db_str(), "ERROR");
        assert_eq!(node_config::NotifyLevel::Info.as_db_str(), "INFO");
    }

    /// camelCase 别名：八个键恒定存在（`render()` 缺变量是 422 硬失败）。
    #[test]
    fn flow_aliases_are_complete() {
        let mut instance = model::InstanceRow {
            id: uuid(1),
            instance_no: "FLW-202611-000001".to_string(),
            def_id: uuid(2),
            version_id: uuid(3),
            version_no: 1,
            flow_code: "pflow2".to_string(),
            flow_name: "采购审批".to_string(),
            biz_type: "PURCHASE_ORDER".to_string(),
            biz_id: None,
            biz_no: Some("PO-1".to_string()),
            title: "采购申请".to_string(),
            status: "PENDING".to_string(),
            initiator_id: uuid(4),
            initiator_name: Some("张三".to_string()),
            initiator_dept_id: None,
            form_data: json!({}),
            result_comment: None,
            auto_passed_nodes: 0,
            round_total: 1,
            prev_instance_id: None,
            revision: 0,
            started_at: chrono::Utc::now(),
            ended_at: None,
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        instance.form_data = json!({"amount": 1});
        let vars = with_flow_aliases(
            json!({"instance_title": "采购申请", "biz_no": "PO-1"}),
            &instance,
            &spec(),
        );
        for key in [
            "flowName",
            "instanceNo",
            "nodeName",
            "initiatorName",
            "title",
            "resultText",
            "comment",
            "urgeCount",
        ] {
            assert!(
                vars.get(key).is_some_and(|value| !value.is_null()),
                "别名 {key} 必须存在且非 null：{vars}"
            );
        }
        assert_eq!(vars["flowName"], json!("采购审批"));
        assert_eq!(vars["nodeName"], json!("通知发起人"));
        assert_eq!(
            vars["instance_title"],
            json!("采购申请"),
            "原有扁平键不能丢"
        );
    }
}

// ==================== 真库用例（引擎全链路，P-FLOW-2 的 6+4+2 条验收） ====================
//
// 与 `model.rs` / `task_bridge.rs` 的真库用例同款：`TASK_DB_URL` 未设置时**整组 SKIP**
// （`cargo test --lib` 在没配库的机器上仍然全绿），设置了就真跑。
//
// 为什么这批用例必须在库里跑：本卡的验收标准是「发起实例 → 发起人真的收到通知」，
// 那要经过 `engine::start` → 快照 → `assignee::resolve` → `notice::dispatch` 的整条链，
// 纯手工桩证明不了任何一步。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::flow::engine;
    use crate::business::flow::graph::{
        SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotEdge, SnapshotNode,
    };
    use serde_json::json;

    /// 真库用例**串行**（共用一个库 + 固定用户名前缀）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局数据库；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("TASK_DB_URL") else {
                    println!("SKIP: 未设置 TASK_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: TASK_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 一致性 uuid（同一 `seed` 永远得到同一个 id，重跑时先删后建）。
    fn uid(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    /// 一条通知节点的快照节点。
    fn notify_spec(
        key: &str,
        notify: serde_json::Value,
        timeout_hours: Option<i32>,
    ) -> SnapshotNode {
        SnapshotNode {
            key: key.to_string(),
            name: "通知发起人".to_string(),
            node_type: "NOTIFY_NODE".to_string(),
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
            timeout_hours,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            config: json!({ "notify": notify }),
            assignees: Vec::new(),
        }
    }

    /// 一个最简单的快照节点（START / END）。
    fn plain_spec(key: &str, name: &str, node_type: &str) -> SnapshotNode {
        SnapshotNode {
            key: key.to_string(),
            name: name.to_string(),
            node_type: node_type.to_string(),
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
            config: json!({}),
            assignees: Vec::new(),
        }
    }

    fn edge(key: &str, from: &str, to: &str) -> SnapshotEdge {
        SnapshotEdge {
            key: key.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            condition: None,
            label: None,
            priority: 0,
            is_default: true,
        }
    }

    /// 一个测试场景（自建 def/version/user/role，跑完删干净）。
    struct Scenario {
        suffix: String,
        def_id: Uuid,
        version_id: Uuid,
        instance_id: Uuid,
        initiator: Uuid,
        user_a: Uuid,
        user_b: Uuid,
        role_code: String,
    }

    impl Scenario {
        async fn new(
            tag: &str,
            seed: u128,
            nodes: Vec<SnapshotNode>,
            edges: Vec<SnapshotEdge>,
        ) -> Self {
            let suffix = format!("p2{tag}");
            let def_id = uid(seed * 100 + 1);
            let version_id = uid(seed * 100 + 2);
            let instance_id = uid(seed * 100 + 3);
            let initiator = uid(seed * 100 + 4);
            let user_a = uid(seed * 100 + 5);
            let user_b = uid(seed * 100 + 6);
            let role_id = uid(seed * 100 + 7);
            let role_code = format!("P2_{tag}").to_ascii_uppercase();

            // 先清理（重跑安全）：drop 顺序按外键反着来
            Self::purge(def_id, version_id, instance_id, &suffix, &role_code).await;

            let snapshot = Snapshot {
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                nodes,
                edges,
            };
            let snapshot_text = serde_json::to_string(&snapshot).expect("快照序列化");
            let graph_hash = "0".repeat(64);

            exec(
                "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status, \
                                           current_version_id, current_version_no) \
                 VALUES (?::uuid, ?, ?, 'PURCHASE_ORDER', 'PUBLISHED', ?::uuid, 1)",
                vec![
                    Value::from(&def_id),
                    Value::Text(format!("pflow2_{}", tag.to_ascii_lowercase())),
                    Value::Text(format!("P-FLOW-2 {tag}")),
                    Value::from(&version_id),
                ],
                "流程定义",
            )
            .await;
            exec(
                "INSERT INTO biz_flow_def_version \
                     (id, def_id, version_no, version_status, is_current, graph_hash, \
                      graph_snapshot, node_count, edge_count, published_at) \
                 VALUES (?::uuid, ?::uuid, 1, 'PUBLISHED', true, ?, ?::jsonb, ?, ?, now())",
                vec![
                    Value::from(&version_id),
                    Value::from(&def_id),
                    Value::Text(graph_hash),
                    // ⚠️ jsonb 位必须 `Value::Json`：`Value::Text` 绑 `?::jsonb` 会
                    // `error serializing parameter`（skill §3 的实测坑）
                    Value::Json(snapshot_text),
                    Value::from(snapshot.nodes.len() as i64),
                    Value::from(snapshot.edges.len() as i64),
                ],
                "版本快照",
            )
            .await;

            exec(
                "INSERT INTO sys_role (id, role_code, role_name, status) VALUES (?::uuid, ?, ?, 1)",
                vec![
                    Value::from(&role_id),
                    Value::Text(role_code.clone()),
                    Value::Text(format!("P-FLOW-2 角色 {tag}")),
                ],
                "角色",
            )
            .await;

            for (id, name) in [
                (initiator, "发起人"),
                (user_a, "指定用户A"),
                (user_b, "指定用户B"),
            ] {
                exec(
                    "INSERT INTO sys_user (id, username, password, real_name, status) \
                     VALUES (?::uuid, ?, 'x', ?, 1)",
                    vec![
                        Value::from(&id),
                        Value::Text(format!("{suffix}_{}", id.simple())),
                        Value::Text(format!("{name}{tag}")),
                    ],
                    "用户",
                )
                .await;
            }
            // user_a / user_b 都挂到这个角色上（ROLE target 用）
            for id in [user_a, user_b] {
                exec(
                    "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::uuid, ?::uuid)",
                    vec![Value::from(&id), Value::from(&role_id)],
                    "用户角色",
                )
                .await;
            }

            Self {
                suffix,
                def_id,
                version_id,
                instance_id,
                initiator,
                user_a,
                user_b,
                role_code,
            }
        }

        /// 发起实例并**把副作用发出去**（与 `handler::drain` 同口径）。
        async fn start(&self) -> engine::ActionOut {
            let req = engine::StartReq {
                instance_id: self.instance_id,
                def_id: self.def_id,
                version_id: self.version_id,
                version_no: 1,
                flow_code: format!("pflow2_{}", self.suffix),
                flow_name: "P-FLOW-2 通知节点".to_string(),
                biz_type: "PURCHASE_ORDER".to_string(),
                biz_id: None,
                biz_no: Some("PO-PFLOW2-0001".to_string()),
                title: "采购申请 PFLOW2".to_string(),
                initiator_id: self.initiator,
                initiator_name: Some("发起人".to_string()),
                initiator_dept_id: None,
                form_data: json!({}),
            };
            let tx = db().begin().await.expect("开启事务");
            let mut out = engine::start(&tx, &req)
                .await
                .expect("引擎动作不应报数据库错误");
            tx.commit().await.expect("提交事务");
            engine::flush_effects(out.effects.take()).await;
            engine::flush_drafts(out.effects.take_drafts()).await;
            out
        }

        /// 节点行状态（第 `round` 轮）。
        async fn node_status(&self, node_key: &str, round: i32) -> Option<String> {
            db().scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance_node \
                  WHERE instance_id = ?::uuid AND node_key = ? AND round = ? LIMIT 1",
                vec![
                    Value::from(&self.instance_id),
                    Value::Text(node_key.to_string()),
                    Value::from(round),
                ],
            )
            .await
            .ok()
            .flatten()
        }

        async fn instance_status(&self) -> String {
            db().scalar_with_params(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&self.instance_id)],
            )
            .await
            .unwrap_or_default()
        }

        /// 本实例的**通知节点**通知（`biz_type = FLOW_NOTIFY`）。
        ///
        /// 必须按 `biz_type` 过滤：流程走完时 `maybe_finish` 还会发一条
        /// 「审批已通过」的普通通知（`biz_type='flow'`），`biz_id` 同样是实例 id。
        /// 不过滤会把两条混在一起，断言就变成「看运气」。
        async fn notices(&self) -> Vec<(String, i64, String)> {
            let rows = db()
                .find_with_params(
                    "SELECT COALESCE(biz_type,'') AS biz_type, receiver_count, \
                            COALESCE(template_code,'') AS template_code \
                       FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY' \
                        AND deleted_at IS NULL ORDER BY created_at",
                    vec![Value::Text(self.instance_id.to_string())],
                )
                .await
                .expect("读通知");
            rows.iter()
                .map(|row| {
                    (
                        row.get_typed::<String>("biz_type").unwrap_or_default(),
                        row.get_typed::<i64>("receiver_count").unwrap_or_default(),
                        row.get_typed::<String>("template_code").unwrap_or_default(),
                    )
                })
                .collect()
        }

        /// 通知节点的接收人（uuid 字符串集合）。
        async fn receivers(&self) -> Vec<String> {
            let rows = db()
                .find_with_params(
                    "SELECT r.user_id::text AS user_id FROM sys_notice_receiver r \
                       JOIN sys_notice n ON n.id = r.notice_id \
                      WHERE n.biz_id = ? AND n.biz_type = 'FLOW_NOTIFY' \
                        AND n.deleted_at IS NULL AND r.is_deleted = FALSE",
                    vec![Value::Text(self.instance_id.to_string())],
                )
                .await
                .expect("读接收人");
            rows.iter()
                .map(|row| row.get_typed::<String>("user_id").unwrap_or_default())
                .collect()
        }

        async fn purge(
            def_id: Uuid,
            version_id: Uuid,
            instance_id: Uuid,
            suffix: &str,
            role_code: &str,
        ) {
            let instance_text = instance_id.to_string();
            for sql in [
                "DELETE FROM sys_notice_receiver WHERE notice_id IN \
                     (SELECT id FROM sys_notice WHERE biz_id = ?)",
                "DELETE FROM sys_notice_delivery WHERE notice_id IN \
                     (SELECT id FROM sys_notice WHERE biz_id = ?)",
                "DELETE FROM sys_notice WHERE biz_id = ?",
            ] {
                let _ = db()
                    .execute_with_params(sql, vec![Value::Text(instance_text.clone())])
                    .await;
            }
            for sql in [
                "DELETE FROM biz_flow_task WHERE instance_id = ?::uuid",
                "DELETE FROM biz_flow_instance_node WHERE instance_id = ?::uuid",
                "DELETE FROM biz_flow_def_version WHERE id = ?::uuid",
                "DELETE FROM biz_flow_instance WHERE id = ?::uuid",
                "DELETE FROM biz_flow_def WHERE id = ?::uuid",
            ] {
                let _ = db()
                    .execute_with_params(sql, vec![Value::from(&instance_id)])
                    .await;
            }
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_flow_def_version WHERE id = ?::uuid",
                    vec![Value::from(&version_id)],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_flow_def WHERE id = ?::uuid",
                    vec![Value::from(&def_id)],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_user_role WHERE user_id IN \
                         (SELECT id FROM sys_user WHERE username LIKE ?)",
                    vec![Value::Text(format!("{suffix}_%"))],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_user WHERE username LIKE ?",
                    vec![Value::Text(format!("{suffix}_%"))],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_role WHERE role_code = ?",
                    vec![Value::Text(role_code.to_string())],
                )
                .await;
        }

        async fn cleanup(&self) {
            Self::purge(
                self.def_id,
                self.version_id,
                self.instance_id,
                &self.suffix,
                &self.role_code,
            )
            .await;
        }
    }

    async fn exec(sql: &str, params: Vec<Value>, what: &str) {
        if let Err(error) = db().execute_with_params(sql, params).await {
            panic!("夹具{what}失败：{error}");
        }
    }

    // ---------- ① ON_ENTER：进入即发，节点立即 APPROVED，驱动下游 ----------
    #[tokio::test]
    async fn on_enter_sends_and_drives_downstream() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "enter",
            1,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {"title": "{{instance.title}} 已到「{{node.name}}」", "content": "单号 {{instance.biz_no}}"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;

        let out = scenario.start().await;
        assert!(
            out.rejected.is_none(),
            "合法配置不该被拒：{:?}",
            out.rejected.map(|e| e.msg)
        );

        // ① 节点立即 APPROVED（非阻塞）
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("APPROVED")
        );
        // ② 驱动下游直到实例结束
        assert_eq!(out.instance_status, "APPROVED");
        assert_eq!(scenario.instance_status().await, "APPROVED");

        // ③ 发起人真的收到一条通知
        let notices = scenario.notices().await;
        assert_eq!(notices.len(), 1, "必须恰好一条通知：{notices:?}");
        assert_eq!(notices[0].0, BIZ_TYPE, "biz_type 必须标记为 FLOW_NOTIFY");
        assert_eq!(notices[0].1, 1, "接收人计数必须回写成 1");
        let receivers = scenario.receivers().await;
        assert_eq!(receivers, vec![scenario.initiator.to_string()]);

        scenario.cleanup().await;
    }

    // ---------- ② ON_LEAVE：节点先通过，再发（幂等键段是 L） ----------
    #[tokio::test]
    async fn on_leave_sends_after_node_passed() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "leave",
            2,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_LEAVE",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {"title": "离开「{{node.name}}」", "content": "单号 {{instance.biz_no}}"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;

        let out = scenario.start().await;
        assert!(out.rejected.is_none());
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("APPROVED")
        );
        assert_eq!(scenario.instance_status().await, "APPROVED");
        assert_eq!(scenario.notices().await.len(), 1, "ON_LEAVE 也必须发");
        // 幂等键的时机段是 L（与 ON_ENTER 的 E 区分开）
        let key: Option<String> = db()
            .scalar_with_params(
                "SELECT idempotency_key FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY' LIMIT 1",
                vec![Value::Text(scenario.instance_id.to_string())],
            )
            .await
            .expect("读幂等键");
        assert!(
            key.unwrap_or_default().starts_with("FLOW:NOTIFY:L:"),
            "ON_LEAVE 的幂等键必须以 FLOW:NOTIFY:L: 开头"
        );

        scenario.cleanup().await;
    }

    // ---------- ③ ON_TIMEOUT：进入不发，超时后才发并放行 ----------
    #[tokio::test]
    async fn on_timeout_fires_only_when_due() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "timeout",
            3,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_TIMEOUT",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {"title": "超时提醒", "content": "单号 {{instance.biz_no}}"}
                    }),
                    Some(4),
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;

        let out = scenario.start().await;
        assert!(out.rejected.is_none());
        // 进入时不发、节点停在 ACTIVE、实例仍在跑
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("ACTIVE")
        );
        assert_eq!(scenario.instance_status().await, "PENDING");
        assert!(
            scenario.notices().await.is_empty(),
            "ON_TIMEOUT 进入时不该发"
        );

        // 没到点 → 扫描捞不到
        let fired = run_due_once().await.expect("扫描不应报错");
        assert_eq!(fired, 0, "没到超时时间不该被放行");
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("ACTIVE")
        );

        // 把激活时间往前挪 5 小时（> timeout_hours=4）→ 应当被捞到并发送
        exec(
            "UPDATE biz_flow_instance_node SET activated_at = now() - interval '5 hours', \
                    created_at = now() - interval '5 hours' \
              WHERE instance_id = ?::uuid AND node_key = 'n_notify'",
            vec![Value::from(&scenario.instance_id)],
            "回拨激活时间",
        )
        .await;
        let fired = run_due_once().await.expect("扫描不应报错");
        assert_eq!(fired, 1, "到点后必须放行一个");
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("APPROVED")
        );
        assert_eq!(scenario.instance_status().await, "APPROVED");
        assert_eq!(scenario.notices().await.len(), 1);

        // 再扫一次不该重复发（节点已 APPROVED）
        let fired = run_due_once().await.expect("扫描不应报错");
        assert_eq!(fired, 0);
        assert_eq!(scenario.notices().await.len(), 1, "不许重复发");

        scenario.cleanup().await;
    }

    // ---------- ④ 投递失败（模板不存在）→ 流程继续，只 warn ----------
    #[tokio::test]
    async fn delivery_failure_does_not_block_flow() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "dfail",
            4,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        // 这个模板在库里不存在 → dispatch 回 404，属于**投递期**故障
                        "template_code": "PFLOW2_NO_SUCH_TEMPLATE"
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;

        let out = scenario.start().await;
        // ★ 关键断言：流程照常走完，发起动作**没有**被通知失败拖垮
        assert!(out.rejected.is_none(), "投递失败不该让发起失败");
        assert_eq!(scenario.instance_status().await, "APPROVED");
        assert_eq!(
            scenario.node_status("n_notify", 1).await.as_deref(),
            Some("APPROVED")
        );
        // 通知确实没落库（失败了），但业务已经提交
        assert!(
            scenario.notices().await.is_empty(),
            "模板不存在时不该有通知行"
        );

        scenario.cleanup().await;
    }

    // ---------- ⑤ 渲染失败（inline 缺变量）→ 400 + 65036，且整笔回滚 ----------
    #[tokio::test]
    async fn render_failure_returns_400_and_rolls_back() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "rfail",
            5,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        // 变量表里没有 form.never_exists → render_paths 硬失败
                        "inline": {"title": "{{form.never_exists}}", "content": "x"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;

        let req = engine::StartReq {
            instance_id: scenario.instance_id,
            def_id: scenario.def_id,
            version_id: scenario.version_id,
            version_no: 1,
            flow_code: "pflow2_rfail".to_string(),
            flow_name: "P-FLOW-2 渲染失败".to_string(),
            biz_type: "PURCHASE_ORDER".to_string(),
            biz_id: None,
            biz_no: Some("PO-PFLOW2-RF".to_string()),
            title: "渲染失败用例".to_string(),
            initiator_id: scenario.initiator,
            initiator_name: Some("发起人".to_string()),
            initiator_dept_id: None,
            form_data: json!({}),
        };
        // 与 handler 同口径：业务拒绝 → 整笔回滚
        let tx = db().begin().await.expect("开启事务");
        let out = engine::start(&tx, &req)
            .await
            .expect("引擎不该报数据库错误");
        if out.rejected.is_some() {
            tx.rollback().await.expect("回滚");
        } else {
            tx.commit().await.expect("提交");
        }
        let error = out.rejected.expect("缺变量必须被拒");
        assert_eq!(error.status, 400, "渲染失败必须是 400（配置错误）");
        assert_eq!(
            error.code,
            crate::shared::error_code::FLOW_NOTIFY_TEMPLATE_INVALID
        );
        assert!(
            error.msg.contains("渲染失败") && error.msg.contains("n_notify"),
            "文案要点名节点：{}",
            error.msg
        );
        // 整笔回滚：实例一行都没有
        let count: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&scenario.instance_id)],
            )
            .await
            .expect("计实例");
        assert_eq!(count, 0, "渲染失败必须整笔回滚");

        scenario.cleanup().await;
    }

    // ---------- ⑦ INITIATOR → 发给发起人 ----------
    #[tokio::test]
    async fn target_initiator_receives() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "tinit",
            7,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {"title": "t", "content": "c"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        assert_eq!(
            scenario.receivers().await,
            vec![scenario.initiator.to_string()],
            "INITIATOR 只发给发起人"
        );
        scenario.cleanup().await;
    }

    // ---------- ⑧ USER → 发给指定用户 ----------
    #[tokio::test]
    async fn target_user_receives() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "tuser",
            8,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "USER", "user_ids": [uid(8 * 100 + 5).to_string()]}],
                        "inline": {"title": "t", "content": "c"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        assert_eq!(
            scenario.receivers().await,
            vec![scenario.user_a.to_string()],
            "USER 只发给指定用户"
        );
        scenario.cleanup().await;
    }

    // ---------- ⑨ ROLE → 发给角色下所有人 ----------
    #[tokio::test]
    async fn target_role_fans_out_members() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "trole",
            9,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "ROLE", "role_codes": ["P2_TROLE"]}],
                        "inline": {"title": "t", "content": "c"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        let mut got = scenario.receivers().await;
        got.sort();
        let mut want = vec![scenario.user_a.to_string(), scenario.user_b.to_string()];
        want.sort();
        assert_eq!(got, want, "ROLE 必须扇出到角色下全部两名成员");
        scenario.cleanup().await;
    }

    // ---------- ⑩ 多个 targets（3 个）→ 全部收到、去重 ----------
    #[tokio::test]
    async fn multiple_targets_all_receive_without_duplicates() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "tmulti",
            10,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [
                            {"type": "INITIATOR"},
                            {"type": "USER", "user_ids": [uid(10 * 100 + 5).to_string()]},
                            {"type": "ROLE", "role_codes": ["P2_TMULTI"]}
                        ],
                        "inline": {"title": "t", "content": "c"}
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        let got = scenario.receivers().await;
        // 发起人 + user_a + user_b（user_a 同时被 USER 与 ROLE 命中 → 去重）
        assert_eq!(got.len(), 3, "三个人各一行，user_a 不许重复：{got:?}");
        for want in [
            scenario.initiator.to_string(),
            scenario.user_a.to_string(),
            scenario.user_b.to_string(),
        ] {
            assert!(got.contains(&want), "缺少接收人 {want}：{got:?}");
        }
        scenario.cleanup().await;
    }

    // ---------- ⑪ template_code 引用已有模板 → 用模板内容渲染 ----------
    #[tokio::test]
    async fn template_code_uses_library_template() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        // 用现成的种子模板 FLOW_TODO（V0125 已种，enabled）
        let scenario = Scenario::new(
            "tcode",
            11,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        "template_code": "FLOW_TODO"
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        let notices = scenario.notices().await;
        assert_eq!(notices.len(), 1, "引用模板必须能发出去：{notices:?}");
        assert_eq!(notices[0].2, "FLOW_TODO", "template_code 要留痕");
        assert_eq!(notices[0].1, 1);
        // 模板渲染出来的标题必须来自 FLOW_TODO，而不是空串
        let title: Option<String> = db()
            .scalar_with_params(
                "SELECT title FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY' LIMIT 1",
                vec![Value::Text(scenario.instance_id.to_string())],
            )
            .await
            .expect("读标题");
        let title = title.unwrap_or_default();
        assert!(!title.trim().is_empty(), "模板必须渲染出标题");
        scenario.cleanup().await;
    }

    // ---------- ⑫ inline + 变量 → 渲染正确 ----------
    #[tokio::test]
    async fn inline_template_renders_instance_variables() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let scenario = Scenario::new(
            "tinline",
            12,
            vec![
                plain_spec("n_start", "开始", "START"),
                notify_spec(
                    "n_notify",
                    json!({
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {
                            "title": "{{instance.biz_no}} 到达「{{node.name}}」",
                            "content": "发起人：{{initiator.name}}｜标题：{{instance.title}}"
                        }
                    }),
                    None,
                ),
                plain_spec("n_end", "结束", "END"),
            ],
            vec![
                edge("e1", "n_start", "n_notify"),
                edge("e2", "n_notify", "n_end"),
            ],
        )
        .await;
        scenario.start().await;
        let row = db()
            .find_with_params(
                "SELECT title, content FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY' LIMIT 1",
                vec![Value::Text(scenario.instance_id.to_string())],
            )
            .await
            .expect("读通知");
        let row = row.first().expect("必须有通知行");
        let title: String = row.get_typed("title").unwrap_or_default();
        let content: String = row.get_typed("content").unwrap_or_default();
        assert_eq!(title, "PO-PFLOW2-0001 到达「通知发起人」");
        assert_eq!(content, "发起人：发起人｜标题：采购申请 PFLOW2");
        scenario.cleanup().await;
    }
}
