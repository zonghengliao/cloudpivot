//! 审批流 · 动态审批人表达式（`EXPRESSION` DSL，批次 D · P1-A1）：**纯逻辑，不碰数据库**。
//!
//! 依据：`docs/批次D-EXPRESSION-DSL-规范.md`
//! §3（语法 / JSON Schema / 上限表 / 求值规则）、§6（四条安全硬约束）、§8.4（公开接口）。
//!
//! # 一句话
//!
//! **把「条件」翻译成「审批人来源」**：一段规则树 `rules[{id, when, then}]`，按序求值，
//! 首条命中即定，输出 `then` 里的目标（`Target`），再翻译成 `assignee.rs` 的查询计划（`SourcePlan`）。
//! 与 `assignee.rs`（把来源翻译成人，碰 DB）职责切开，边界就是「这个文件里一行 SQL/一行 `await` 都没有」。
//!
//! # 为什么 `when` 子树不另写解析器（D-E2）
//!
//! `when` 的 JSON 形状与条件边 `biz_flow_edge.condition_expr` **逐字相同**，直接复用
//! [`graph::parse_condition`] + [`graph::eval`] + [`graph::field_allowed`] 的白名单与上限表。
//! 再造一套 = 第二个求值器 + 第二套注入面 + 两份上限 + 两份错误文案，而用户看到的东西完全一样。
//!
//! # 四条硬约束（§6.1，不可协商）
//!
//! | # | 约束 | 本文件的保证 |
//! |---|---|---|
//! | S1 | 禁止任意代码执行 | 只有 `Condition` / `Target` 两个强类型 enum，枚举之外没有执行面 |
//! | S2 | 求值是纯函数 | [`eval_expression`] 入参只有 `(&Expression, &EvalContext)`：无 `await`、无 `&mut`、无 IO |
//! | S3 | 有界 | 全部是**结构性上限**（规则 ≤16、`then` ≤5、深度 ≤8、单条节点 ≤64、总节点 ≤256），不用计时器 |
//! | S4 | 失败不静默 | 解析失败返回中文 `Err`（保存期 65033 / 运行期 65032）；求值出错记 trace 按「不成立」继续 |
//!
//! # 求值语义（§3.6，D-E6 / D-E7）
//!
//! 1. **首条命中即定**，不顺延：`rules[0]` 成立就返回，后面的 `when` **根本不被求值**（含短路，见 E-T02/E-T05）。
//! 2. **求值出错不算命中也不算失败**：字段缺失（`Missing`）/ 类型不匹配（`TypeMismatch`）→ 该规则按「不成立」处理，
//!    记一行 trace，继续下一条。
//! 3. **全部不命中**：有 `otherwise` → [`EvalOutcome::Otherwise`]；没有 → [`EvalOutcome::NoMatch`]
//!    （调用方走 `empty_assignee` 策略：SKIP / ERROR / FALLBACK_ADMIN）。
//! 4. **命中但解析出 0 人**是 `assignee.rs` 的事（`empty_assignee` 策略），**不顺延到下一条规则**。
//!
//! # trace 为什么走旁路参数（§8.4）
//!
//! [`eval_expression`] **不返回 `Result`**：返回值只表达「命中了谁 / 没命中」，
//! 调用方**没有 `?` 可写**，也就不可能出现「把错误当成空审批人 → 节点自动通过」的静默失效。
//! 排障细节（`Missing: task.priority`、`命中 r_big`）走 `trace: &mut Vec<String>`，
//! 用 [`eval_expression_with_trace`] 拿；只关心结果时用 [`eval_expression`]。
//!
//! # 本卡范围
//!
//! 只做「迁移 + 本文件」。`assignee.rs` 的 `EXPRESSION` 分支、`graph.rs` 的 R16、`dto.rs` / `handler.rs` 的
//! 保存期校验属于 P1-A2 —— 那些位置直接调本文件的 [`parse_expression`] / [`parse_config`] / [`parse_snapshot_node`] 即可。
//! `PREV_APPROVER`（能力 B）本期只做「保留词 + 解析期拒绝」。

use std::collections::BTreeSet;

use nik::uuid::Uuid;
use serde_json::{Map, Value};

use crate::business::flow::assignee::{SourcePlan, TaskParty};
use crate::business::flow::graph::{self, Condition, EvalContext, SnapshotNode};

// ==================== 上限（§3.4，全部是结构性上限） ====================

/// 表达式版本（`version` 字段的唯一合法值）。版本不认识 ⇒ 解析失败 ⇒ 运行期 501 + 65032。
pub const EXPRESSION_VERSION: u32 = 1;

/// 规则数上限（`rules` 数组长度）。
pub const MAX_EXPR_RULES: usize = 16;

/// 单条规则的 `then` / `otherwise` 目标数上限。
pub const MAX_EXPR_TARGETS: usize = 5;

/// 整个表达式的 `when` 节点总数上限（防 16 × 64 = 1024 的叠加）。
pub const MAX_EXPR_WHEN_NODES: usize = 256;

/// `user_ids` / `dept_ids` 单项上限。
pub const MAX_EXPR_IDS: usize = 20;

/// `role_codes` 单项上限。
pub const MAX_EXPR_ROLES: usize = 10;

/// 规则 id 最短长度（Schema 的 `^[a-z][a-z0-9_]{1,31}$`）。
pub const MIN_RULE_ID_CHARS: usize = 2;

/// 规则 id 最长长度。
pub const MAX_RULE_ID_CHARS: usize = 32;

/// `INITIATOR_SUPERIOR.levels` 上限（与 `biz_flow_node.superior_levels` 的 1..10 一致）。
pub const MAX_SUPERIOR_LEVELS: i16 = 10;

/// 本期不开放的保留词（能力 B：上一节点审批人）。
pub const RESERVED_PREV_APPROVER: &str = "PREV_APPROVER";

/// `otherwise` 命中时写进 `biz_flow_task.assignee_rule_id` 的固定值（D-E5）。
pub const OTHERWISE_RULE_ID: &str = "otherwise";

/// 十种目标来源的合法取值（文案与解析共用一份，避免两处漂移）。
const TARGET_TYPES: [&str; 10] = [
    "USER",
    "ROLE",
    "DEPT_LEADER",
    "INITIATOR",
    "INITIATOR_SUPERIOR",
    "TASK_OWNER",
    "TASK_CREATOR",
    "TASK_ASSIGNER",
    "FORM_VAR",
    "ADMIN",
];

// ==================== 强类型表示（§8.4） ====================

/// 一段表达式（`biz_flow_node.config.expression` 反序列化后的形态）。
#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    /// 版本（本文件只认 [`EXPRESSION_VERSION`]）。
    pub version: u32,
    /// 规则（数组顺序 = 优先级；首条命中即定）。
    pub rules: Vec<ExprRule>,
    /// 兜底目标（全部规则不成立时使用；`None` ⇒ 走 `empty_assignee` 策略）。
    pub otherwise: Option<Vec<Target>>,
}

/// 一条规则。
#[derive(Debug, Clone, PartialEq)]
pub struct ExprRule {
    /// 规则 id（节点内唯一，写进 `biz_flow_task.assignee_rule_id`）。
    pub id: String,
    /// 条件（与条件边的 `Condition` 同一套类型与白名单）。
    pub when: Condition,
    /// 命中后的审批人目标（多个 = **并集**，不是会签/或签）。
    pub then: Vec<Target>,
}

/// 审批人目标（10 种来源，与 §3.5 表一一对应；`PREV_APPROVER` 在解析期就被拒绝，进不来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 指定用户（`user_ids`，UUID 文本）。
    User {
        /// 用户 id 列表。
        user_ids: Vec<Uuid>,
    },
    /// 指定角色（`role_codes`，角色**编码**而不是 id，跨环境稳定）。
    Role {
        /// 角色编码列表（运行时由 `assignee.rs` 查 code→id，见 §8.3）。
        role_codes: Vec<String>,
    },
    /// 部门负责人（`dept_ids` 为空 ⇒ 发起人部门）。
    DeptLeader {
        /// 部门 id 列表（可空）。
        dept_ids: Vec<Uuid>,
    },
    /// 发起人本人。
    Initiator,
    /// 发起人主管（上溯层数）。
    InitiatorSuperior {
        /// 上溯层数（1..=10，默认 1）。
        levels: i16,
    },
    /// 关联任务负责人。
    TaskOwner,
    /// 关联任务创建人。
    TaskCreator,
    /// 关联任务分派人。
    TaskAssigner,
    /// 表单变量（`form_data[form_var_key]`）。
    FormVar {
        /// 表单字段名。
        form_var_key: String,
    },
    /// 平台超管。
    Admin,
}

impl Target {
    /// 来源类型字符串（与 `biz_flow_node.assignee_type` / `AssigneeKind::as_str` 同口径）。
    pub fn type_str(&self) -> &'static str {
        match self {
            Target::User { .. } => "USER",
            Target::Role { .. } => "ROLE",
            Target::DeptLeader { .. } => "DEPT_LEADER",
            Target::Initiator => "INITIATOR",
            Target::InitiatorSuperior { .. } => "INITIATOR_SUPERIOR",
            Target::TaskOwner => "TASK_OWNER",
            Target::TaskCreator => "TASK_CREATOR",
            Target::TaskAssigner => "TASK_ASSIGNER",
            Target::FormVar { .. } => "FORM_VAR",
            Target::Admin => "ADMIN",
        }
    }
}

/// 求值结论（§3.6 的 8 种情形收敛成三种）。
#[derive(Debug, Clone, PartialEq)]
pub enum EvalOutcome {
    /// 命中某条规则：展开 `targets`（顺序 = JSON 数组顺序，影响去重优先级）。
    Hit {
        /// 命中的规则 id（写 `biz_flow_task.assignee_rule_id`）。
        rule_id: String,
        /// 该规则的 `then`。
        targets: Vec<Target>,
    },
    /// 全部规则不成立但配了 `otherwise`。
    Otherwise {
        /// 兜底目标。
        targets: Vec<Target>,
    },
    /// 全部规则不成立且没有 `otherwise` ⇒ 调用方走 `empty_assignee` 策略。
    NoMatch,
}

impl EvalOutcome {
    /// 命中的规则 id（`NoMatch` → `None`；`Otherwise` → [`OTHERWISE_RULE_ID`] 由调用方写）。
    pub fn rule_id(&self) -> Option<&str> {
        match self {
            EvalOutcome::Hit { rule_id, .. } => Some(rule_id),
            EvalOutcome::Otherwise { .. } => Some(OTHERWISE_RULE_ID),
            EvalOutcome::NoMatch => None,
        }
    }

    /// 需要展开的目标（`NoMatch` → 空切片）。
    pub fn targets(&self) -> &[Target] {
        match self {
            EvalOutcome::Hit { targets, .. } | EvalOutcome::Otherwise { targets } => targets,
            EvalOutcome::NoMatch => &[],
        }
    }
}

// ==================== 目标 → 查询计划（§8.2 / §8.3） ====================

/// 翻译 `Target` 需要的运行期上下文（发起人 / 部门）。
///
/// 关联任务（`TASK_*`）与表单变量（`FORM_VAR`）的取值都在 `assignee.rs` 的 `load_plan` 里做
/// （它们要读 `biz_task` / `form_data`），本文件只负责「翻译成什么计划」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveContext {
    /// 发起人部门（`biz_flow_instance.initiator_dept_id`）：`DEPT_LEADER` 未指定部门时用它。
    pub initiator_dept_id: Option<Uuid>,
}

/// 一个已翻译的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// 直接可执行的查询计划（复用 `assignee.rs` 的 `SourcePlan`）。
    Plan(SourcePlan),
    /// 角色**编码**：需要 `assignee.rs` 先查 `sys_role` 的 code→id，再走既有的 `SourcePlan::Roles`（§8.3 方案 B）。
    ///
    /// 放在这里而不是直接给 `SourcePlan::Roles(ids)` 的原因：查库不是纯函数，本文件一行 SQL 都没有。
    RoleCodes(Vec<String>),
}

/// 翻译结果（顺序 = `then` 的 JSON 数组顺序，影响 `finalize()` 的去重优先级）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedTargets {
    /// 每个目标翻译后的一项（不合并、不去重 —— 去重是 `finalize()` 的职责）。
    pub targets: Vec<ResolvedTarget>,
    /// 翻译痕迹（写 `Resolution::trace`）。
    pub notes: Vec<String>,
}

impl ResolvedTargets {
    /// 目标个数。
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// 是否一个目标都没有（命中但 0 目标 ⇒ 空审批人）。
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

// ==================== 解析（静态校验，保存期 / 发布期 / 运行期共用） ====================

/// 解析表达式正文（`config.expression` 的值）。
///
/// 返回中文原因，直接可拼进 65033 的文案；文案里带「第 N 条规则（r_x）」的定位信息。
pub fn parse_expression(value: &Value) -> Result<Expression, String> {
    let root = expect_object(value, "表达式")?;
    deny_unknown_keys(root, &["version", "rules", "otherwise"], "表达式")?;

    let version = parse_version(root)?;
    let rules_json = root
        .get("rules")
        .ok_or_else(|| "表达式缺少 rules 数组（至少 1 条规则）".to_string())?
        .as_array()
        .ok_or_else(|| "表达式的 rules 必须是数组".to_string())?;
    if rules_json.is_empty() {
        return Err("表达式至少要有 1 条规则".to_string());
    }
    if rules_json.len() > MAX_EXPR_RULES {
        return Err(format!(
            "规则数超过 {MAX_EXPR_RULES} 条（当前 {} 条）",
            rules_json.len()
        ));
    }

    let mut rule_ids: BTreeSet<&str> = BTreeSet::new();
    let mut rules: Vec<ExprRule> = Vec::with_capacity(rules_json.len());
    let mut when_nodes = 0usize;
    for (index, rule_json) in rules_json.iter().enumerate() {
        let position = index + 1;
        let rule = expect_object(rule_json, &format!("第 {position} 条规则"))?;
        deny_unknown_keys(
            rule,
            &["id", "when", "then"],
            &format!("第 {position} 条规则"),
        )?;
        let id = parse_rule_id(rule, position)?;
        if !rule_ids.insert(id) {
            return Err(format!("规则 id 重复：{id}"));
        }

        let when_json = rule
            .get("when")
            .ok_or_else(|| format!("第 {position} 条规则（{id}）缺少条件（when）"))?;
        let nodes = count_condition_nodes(when_json);
        if nodes > graph::MAX_CONDITION_NODES {
            return Err(format!(
                "第 {position} 条规则（{id}）的条件节点数超过 {} 个（当前 {nodes} 个）",
                graph::MAX_CONDITION_NODES
            ));
        }
        when_nodes += nodes;
        if when_nodes > MAX_EXPR_WHEN_NODES {
            return Err(format!(
                "整条表达式的条件节点总数超过 {MAX_EXPR_WHEN_NODES} 个（当前 {when_nodes} 个，请拆成几个审批节点）"
            ));
        }
        let when = graph::parse_condition(when_json)
            .map_err(|reason| format!("第 {position} 条规则（{id}）的条件不合法：{reason}"))?;

        let then_json = rule
            .get("then")
            .ok_or_else(|| format!("第 {position} 条规则（{id}）缺少审批人（then）"))?;
        let then = parse_targets(then_json, &format!("第 {position} 条规则（{id}）"))?;

        rules.push(ExprRule {
            id: id.to_string(),
            when,
            then,
        });
    }

    let otherwise = match root.get("otherwise") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_targets(value, "otherwise")?),
    };

    Ok(Expression {
        version,
        rules,
        otherwise,
    })
}

/// 从节点配置（`biz_flow_node.config` / 快照 `SnapshotNode.config`）里取 `expression` 并解析。
///
/// `EXPRESSION` 但 `config` 里没有 `expression` 是**配置错误**（不是「空审批人」），必须报错让用户去配规则。
pub fn parse_config(config: &Value) -> Result<Expression, String> {
    match config.get("expression") {
        Some(value) if !value.is_null() => parse_expression(value),
        _ => Err(
            "审批人来源选择了表达式，但 config 里没有 expression —— 请先配置规则（至少 1 条 when/then）"
                .to_string(),
        ),
    }
}

/// 从快照节点取 `config.expression` 并解析（运行期 / 发布期 R16 共用）。
pub fn parse_snapshot_node(node: &SnapshotNode) -> Result<Expression, String> {
    parse_config(&node.config)
}

fn parse_version(root: &Map<String, Value>) -> Result<u32, String> {
    let raw = root.get("version").ok_or_else(|| {
        format!("表达式缺少 version 字段（本期只支持 version={EXPRESSION_VERSION}）")
    })?;
    let version = raw
        .as_u64()
        .ok_or_else(|| format!("表达式的 version 必须是整数 {EXPRESSION_VERSION}"))?;
    if version != u64::from(EXPRESSION_VERSION) {
        return Err(format!(
            "不支持的表达式版本 {version}（本期只支持 version={EXPRESSION_VERSION}，可能是更新的数据或旧版本代码）"
        ));
    }
    Ok(EXPRESSION_VERSION)
}

fn parse_rule_id(rule: &Map<String, Value>, position: usize) -> Result<&str, String> {
    let raw = rule
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("第 {position} 条规则缺少 id（规则标识，如 r_big）"))?;
    let length = raw.chars().count();
    let mut chars = raw.chars();
    let head_ok = chars.next().is_some_and(|first| first.is_ascii_lowercase());
    if !head_ok
        || !(MIN_RULE_ID_CHARS..=MAX_RULE_ID_CHARS).contains(&length)
        || !raw
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(format!(
            "第 {position} 条规则的 id「{raw}」格式不合法（要求小写字母开头，只含小写字母/数字/下划线，长度 {MIN_RULE_ID_CHARS}–{MAX_RULE_ID_CHARS}）"
        ));
    }
    Ok(raw)
}

fn parse_targets(value: &Value, where_desc: &str) -> Result<Vec<Target>, String> {
    let items = value.as_array().ok_or_else(|| {
        format!("{where_desc}的审批人（then/otherwise）必须是数组，如 [{{\"type\":\"ROLE\",\"role_codes\":[\"FINANCE_DIRECTOR\"]}}]")
    })?;
    if items.is_empty() {
        return Err(format!("{where_desc}的审批人不能为空（至少 1 个）"));
    }
    if items.len() > MAX_EXPR_TARGETS {
        return Err(format!(
            "{where_desc}的审批人数超过 {MAX_EXPR_TARGETS} 个（当前 {} 个）",
            items.len()
        ));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| parse_target(item, &format!("{where_desc}第 {} 个审批人", index + 1)))
        .collect()
}

fn parse_target(value: &Value, where_desc: &str) -> Result<Target, String> {
    let map = expect_object(value, where_desc)?;
    let raw_type = map
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .ok_or_else(|| format!("{where_desc}缺少 type（允许 {}）", TARGET_TYPES.join(" / ")))?
        .to_ascii_uppercase();

    if raw_type == RESERVED_PREV_APPROVER {
        return Err(format!(
            "{where_desc}的来源 {RESERVED_PREV_APPROVER} 本版本未开放（P2 能力 B：取上一节点审批人），请先改用其它来源"
        ));
    }
    if !TARGET_TYPES.contains(&raw_type.as_str()) {
        return Err(format!(
            "{where_desc}的来源「{raw_type}」不支持（允许 {}）",
            TARGET_TYPES.join(" / ")
        ));
    }

    match raw_type.as_str() {
        "USER" => {
            deny_unknown_keys(map, &["type", "user_ids"], where_desc)?;
            let ids = required_uuid_array(map, "user_ids", where_desc)?;
            Ok(Target::User { user_ids: ids })
        }
        "ROLE" => {
            // role_ids 是上一版草案的写法：专开一条文案，避免用户拿到「未知字段 role_ids」还不知道该用哪个键。
            if map.contains_key("role_ids") {
                return Err(format!(
                    "{where_desc}请用 role_codes（角色编码）而不是 role_ids（角色 id）：id 由 gen_random_uuid() 生成，跨环境会失配"
                ));
            }
            deny_unknown_keys(map, &["type", "role_codes"], where_desc)?;
            let codes = required_str_array(map, "role_codes", where_desc, MAX_EXPR_ROLES)?;
            Ok(Target::Role { role_codes: codes })
        }
        "DEPT_LEADER" => {
            deny_unknown_keys(map, &["type", "dept_ids"], where_desc)?;
            let dept_ids = optional_uuid_array(map, "dept_ids", where_desc)?.unwrap_or_default();
            Ok(Target::DeptLeader { dept_ids })
        }
        "INITIATOR" => {
            deny_unknown_keys(map, &["type"], where_desc)?;
            Ok(Target::Initiator)
        }
        "INITIATOR_SUPERIOR" => {
            deny_unknown_keys(map, &["type", "levels"], where_desc)?;
            Ok(Target::InitiatorSuperior {
                levels: parse_levels(map, where_desc)?,
            })
        }
        "TASK_OWNER" => {
            deny_unknown_keys(map, &["type"], where_desc)?;
            Ok(Target::TaskOwner)
        }
        "TASK_CREATOR" => {
            deny_unknown_keys(map, &["type"], where_desc)?;
            Ok(Target::TaskCreator)
        }
        "TASK_ASSIGNER" => {
            deny_unknown_keys(map, &["type"], where_desc)?;
            Ok(Target::TaskAssigner)
        }
        "FORM_VAR" => {
            deny_unknown_keys(map, &["type", "form_var_key"], where_desc)?;
            let key = map
                .get("form_var_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|key| is_form_var_key(key))
                .ok_or_else(|| {
                    format!(
                        "{where_desc}缺少 form_var_key（表单字段名，字母或下划线开头，最长 64）"
                    )
                })?;
            Ok(Target::FormVar {
                form_var_key: key.to_string(),
            })
        }
        // 十种来源在上面那个 contains 判定里已收口，这里只剩 ADMIN
        _ => {
            deny_unknown_keys(map, &["type"], where_desc)?;
            Ok(Target::Admin)
        }
    }
}

fn parse_levels(map: &Map<String, Value>, where_desc: &str) -> Result<i16, String> {
    let Some(raw) = map.get("levels") else {
        return Ok(1);
    };
    if raw.is_null() {
        return Ok(1);
    }
    let levels = raw
        .as_i64()
        .ok_or_else(|| format!("{where_desc}的 levels 必须是整数 1–{MAX_SUPERIOR_LEVELS}"))?;
    let max = i64::from(MAX_SUPERIOR_LEVELS);
    if !(1..=max).contains(&levels) {
        return Err(format!(
            "{where_desc}的 levels 必须在 1–{MAX_SUPERIOR_LEVELS} 之间（当前 {levels}）"
        ));
    }
    Ok(levels as i16)
}

fn required_str_array(
    map: &Map<String, Value>,
    key: &str,
    where_desc: &str,
    max: usize,
) -> Result<Vec<String>, String> {
    optional_str_array(map, key, where_desc, max)?
        .ok_or_else(|| format!("{where_desc}缺少 {key}（1–{max} 项）"))
}

fn optional_str_array(
    map: &Map<String, Value>,
    key: &str,
    where_desc: &str,
    max: usize,
) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = map.get(key) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let items = raw
        .as_array()
        .ok_or_else(|| format!("{where_desc}的 {key} 必须是字符串数组"))?;
    if items.is_empty() {
        return Err(format!("{where_desc}的 {key} 不能为空数组"));
    }
    if items.len() > max {
        return Err(format!(
            "{where_desc}的 {key} 超过 {max} 项（当前 {} 项）",
            items.len()
        ));
    }
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let text = item.as_str().ok_or_else(|| {
            format!(
                "{where_desc}的 {key} 只允许字符串（当前是{}）",
                json_kind(item)
            )
        })?;
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("{where_desc}的 {key} 里不能有空字符串"));
        }
        out.push(text.to_string());
    }
    Ok(Some(out))
}

fn required_uuid_array(
    map: &Map<String, Value>,
    key: &str,
    where_desc: &str,
) -> Result<Vec<Uuid>, String> {
    optional_uuid_array(map, key, where_desc)?
        .ok_or_else(|| format!("{where_desc}缺少 {key}（1–{MAX_EXPR_IDS} 个 UUID）"))
}

fn optional_uuid_array(
    map: &Map<String, Value>,
    key: &str,
    where_desc: &str,
) -> Result<Option<Vec<Uuid>>, String> {
    let Some(items) = optional_str_array(map, key, where_desc, MAX_EXPR_IDS)? else {
        return Ok(None);
    };
    let mut out: Vec<Uuid> = Vec::with_capacity(items.len());
    for raw in items {
        let id = Uuid::parse_str(&raw)
            .map_err(|_| format!("{where_desc}的 {key} 里的「{raw}」不是合法 UUID"))?;
        out.push(id);
    }
    Ok(Some(out))
}

fn expect_object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what}必须是 JSON 对象（当前是{}）", json_kind(value)))
}

fn deny_unknown_keys(map: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{what}含未知字段 {key}（允许的键：{}）",
                allowed.join(" / ")
            ));
        }
    }
    Ok(())
}

/// `when` 子树的节点数（对象即一个节点；只沿 `args` 下探）。
fn count_condition_nodes(value: &Value) -> usize {
    let Value::Object(map) = value else {
        return 0;
    };
    let children: usize = map
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().map(count_condition_nodes).sum())
        .unwrap_or(0);
    1 + children
}

fn is_form_var_key(key: &str) -> bool {
    let mut chars = key.chars();
    let head_ok = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    head_ok && key.chars().count() <= 64 && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

// ==================== 求值（纯函数，无 IO / 无 await / 无副作用） ====================

/// 求值（只关心「命中谁」时用它；trace 丢弃）。
pub fn eval_expression(expr: &Expression, ctx: &EvalContext) -> EvalOutcome {
    let mut trace: Vec<String> = Vec::new();
    eval_expression_with_trace(expr, ctx, &mut trace)
}

/// 求值 + 收集 trace（排障 / 详情页「为什么是他」用）。
///
/// trace 的三种行（与 §3.6 的表格一一对应）：
/// - `表达式命中规则 r_big`
/// - `规则 r1 求值失败（Missing: form.not_filled）→ 按不成立处理`
/// - `无规则命中且无兜底 → 空审批人` / `无规则命中，走 otherwise`
pub fn eval_expression_with_trace(
    expr: &Expression,
    ctx: &EvalContext,
    trace: &mut Vec<String>,
) -> EvalOutcome {
    for rule in &expr.rules {
        match graph::eval(&rule.when, ctx) {
            Ok(true) => {
                trace.push(format!("表达式命中规则 {}", rule.id));
                return EvalOutcome::Hit {
                    rule_id: rule.id.clone(),
                    targets: rule.then.clone(),
                };
            }
            // 不成立：不记 trace（否则 16 条规则会把日志刷满，§3.6 第 2 行标的就是「可选，debug 级」）
            Ok(false) => {}
            // 出错 ≠ 失败：按「不成立」处理继续下一条，但必须留痕（S4 失败不静默）
            Err(error) => trace.push(format!(
                "规则 {} 求值失败（{error}）→ 按不成立处理",
                rule.id
            )),
        }
    }

    match &expr.otherwise {
        Some(targets) => {
            trace.push(format!("无规则命中，走 {OTHERWISE_RULE_ID}"));
            EvalOutcome::Otherwise {
                targets: targets.clone(),
            }
        }
        None => {
            trace.push("无规则命中且无兜底 → 空审批人".to_string());
            EvalOutcome::NoMatch
        }
    }
}

// ==================== 目标 → 查询计划 ====================

/// 把命中规则的 `then`（或 `otherwise`）翻译成查询计划（纯内存，不查库）。
///
/// - 顺序 = JSON 数组顺序（**不要**排序/合并：顺序影响 `finalize()` 的去重优先级）；
/// - `ROLE` 产出 [`ResolvedTarget::RoleCodes`]：code→id 的查询在 `assignee.rs`（§8.3 方案 B）；
/// - `DEPT_LEADER` 的 `dept_ids` 为空时**回落发起人部门**（`ctx.initiator_dept_id`），
///   发起人也没有部门就得到空部门列表（⇒ 该来源解析 0 人，由 `empty_assignee` 策略兜底）。
pub fn resolve_targets(targets: &[Target], ctx: &ResolveContext) -> ResolvedTargets {
    let mut resolved = ResolvedTargets::default();
    for target in targets {
        match target {
            Target::User { user_ids } => resolved
                .targets
                .push(ResolvedTarget::Plan(SourcePlan::Users(user_ids.clone()))),
            Target::Role { role_codes } => resolved
                .targets
                .push(ResolvedTarget::RoleCodes(role_codes.clone())),
            Target::DeptLeader { dept_ids } => {
                let ids: Vec<Uuid> = if dept_ids.is_empty() {
                    match ctx.initiator_dept_id {
                        Some(dept_id) => {
                            resolved.notes.push(format!(
                                "DEPT_LEADER 未指定部门 → 按发起人部门 {dept_id} 解析"
                            ));
                            vec![dept_id]
                        }
                        None => {
                            resolved.notes.push(
                                "DEPT_LEADER 未指定部门且发起人没有部门 → 该来源解析 0 人"
                                    .to_string(),
                            );
                            Vec::new()
                        }
                    }
                } else {
                    dept_ids.clone()
                };
                resolved
                    .targets
                    .push(ResolvedTarget::Plan(SourcePlan::DeptLeaders(ids)));
            }
            Target::Initiator => resolved
                .targets
                .push(ResolvedTarget::Plan(SourcePlan::Initiator)),
            Target::InitiatorSuperior { levels } => resolved
                .targets
                .push(ResolvedTarget::Plan(SourcePlan::InitiatorSuperior(*levels))),
            Target::TaskOwner => {
                resolved
                    .targets
                    .push(ResolvedTarget::Plan(SourcePlan::TaskParty(
                        TaskParty::Owner,
                    )))
            }
            Target::TaskCreator => {
                resolved
                    .targets
                    .push(ResolvedTarget::Plan(SourcePlan::TaskParty(
                        TaskParty::Creator,
                    )))
            }
            Target::TaskAssigner => {
                resolved
                    .targets
                    .push(ResolvedTarget::Plan(SourcePlan::TaskParty(
                        TaskParty::Assigner,
                    )))
            }
            Target::FormVar { form_var_key } => {
                resolved
                    .targets
                    .push(ResolvedTarget::Plan(SourcePlan::FormVar(
                        form_var_key.clone(),
                    )))
            }
            Target::Admin => resolved
                .targets
                .push(ResolvedTarget::Plan(SourcePlan::Admins)),
        }
    }
    resolved
}

// ==================== 单元测试（§8.8，24 条） ====================

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::business::flow::assignee::{Candidate, EmptyPolicy, SelfPolicy, finalize};
    use crate::business::flow::graph::AssigneeKind;

    // ---------- 造 JSON 的小工具 ----------

    fn cmp(field: &str, op: &str, value: Value) -> Value {
        json!({ "op": "cmp", "field": field, "cmp": op, "value": value })
    }

    fn unary(field: &str, op: &str) -> Value {
        json!({ "op": "cmp", "field": field, "cmp": op })
    }

    fn and(args: Vec<Value>) -> Value {
        json!({ "op": "and", "args": args })
    }

    fn or(args: Vec<Value>) -> Value {
        json!({ "op": "or", "args": args })
    }

    fn not(inner: Value) -> Value {
        json!({ "op": "not", "args": [inner] })
    }

    fn rule(id: &str, when: Value, then: Value) -> Value {
        json!({ "id": id, "when": when, "then": then })
    }

    fn expression(rules: Vec<Value>, otherwise: Option<Value>) -> Value {
        let mut root = json!({ "version": 1, "rules": rules });
        if let Some(otherwise) = otherwise {
            root["otherwise"] = otherwise;
        }
        root
    }

    fn role(codes: &[&str]) -> Value {
        json!({ "type": "ROLE", "role_codes": codes })
    }

    fn admin() -> Value {
        json!({ "type": "ADMIN" })
    }

    /// 一条「永远成立」的 when（`form.amount >= 0`）。
    fn always(form_amount: i64) -> Value {
        cmp("form.amount", "gte", json!(form_amount))
    }

    fn parse(value: Value) -> Expression {
        parse_expression(&value).unwrap_or_else(|reason| panic!("期望解析成功，实际失败：{reason}"))
    }

    fn parse_err(value: Value) -> String {
        match parse_expression(&value) {
            Ok(_) => panic!("期望解析失败，实际成功"),
            Err(reason) => reason,
        }
    }

    fn ctx_with(form: Value) -> EvalContext {
        let mut context = EvalContext::default();
        if let Value::Object(map) = form {
            context.form = map;
        }
        context
    }

    /// 构造一个刚好 `nodes` 个节点的 `and` 树（用来测上限，深度保持在 8 层内）。
    fn condition_with_nodes(nodes: usize) -> Value {
        assert!(nodes >= 2, "至少 1 个 and + 1 个 cmp");
        let mut remaining = nodes - 1;
        let mut args: Vec<Value> = Vec::new();
        while remaining > 0 {
            if remaining <= graph::MAX_CONDITION_ARGS {
                for index in 0..remaining {
                    args.push(cmp("form.f", "eq", json!(index)));
                }
                remaining = 0;
            } else {
                // 一个子 and 吃掉剩下的（减去 15 个叶子 cmp），保证 args 不超 16
                let sub = remaining - (graph::MAX_CONDITION_ARGS - 1);
                args.push(condition_with_nodes(sub));
                for index in 0..(graph::MAX_CONDITION_ARGS - 1) {
                    args.push(cmp("form.f", "eq", json!(index)));
                }
                remaining = 0;
            }
        }
        and(args)
    }

    fn trace_of(expr: &Expression, ctx: &EvalContext) -> (EvalOutcome, Vec<String>) {
        let mut trace: Vec<String> = Vec::new();
        let outcome = eval_expression_with_trace(expr, ctx, &mut trace);
        (outcome, trace)
    }

    // ---------- E-T01 ~ E-T16：解析 + 求值 ----------

    /// E-T01 单条规则命中：`amount=12000` 命中 `gt 10000`。
    #[test]
    fn e_t01_single_rule_hit() {
        let expr = parse(expression(
            vec![rule(
                "r_big",
                cmp("form.amount", "gt", json!(10000)),
                json!([role(&["FINANCE_DIRECTOR"])]),
            )],
            None,
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 12000 })));
        assert_eq!(
            outcome,
            EvalOutcome::Hit {
                rule_id: "r_big".to_string(),
                targets: vec![Target::Role {
                    role_codes: vec!["FINANCE_DIRECTOR".to_string()]
                }],
            }
        );
        assert_eq!(outcome.rule_id(), Some("r_big"));
    }

    /// E-T02 首条命中即停：第 2 条的 `when` 引用 `task.priority`（ctx 里没有），
    /// 若被求值必然留下 Missing trace —— 断言「没有」证明第二根本条没被求值。
    #[test]
    fn e_t02_first_match_wins_and_second_rule_not_evaluated() {
        let expr = parse(expression(
            vec![
                rule(
                    "r_first",
                    cmp("form.amount", "gt", json!(10000)),
                    json!([role(&["FINANCE_DIRECTOR"])]),
                ),
                rule(
                    "r_second",
                    cmp("task.priority", "eq", json!("P0")),
                    json!([admin()]),
                ),
            ],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": 12000 })));
        assert_eq!(outcome.rule_id(), Some("r_first"));
        assert!(
            trace.iter().all(|line| !line.contains("Missing")),
            "第二条规则不该被求值：{trace:?}"
        );
    }

    /// E-T03 全部不成立 + 有 `otherwise`。
    #[test]
    fn e_t03_no_match_with_otherwise() {
        let expr = parse(expression(
            vec![rule(
                "r_big",
                cmp("form.amount", "gt", json!(10000)),
                json!([role(&["FINANCE_DIRECTOR"])]),
            )],
            Some(json!([{ "type": "DEPT_LEADER" }])),
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 100 })));
        assert_eq!(
            outcome,
            EvalOutcome::Otherwise {
                targets: vec![Target::DeptLeader { dept_ids: vec![] }],
            }
        );
        assert_eq!(outcome.rule_id(), Some(OTHERWISE_RULE_ID));
    }

    /// E-T04 全部不成立 + 无 `otherwise` → `NoMatch`，trace 必须点名「无兜底」。
    #[test]
    fn e_t04_no_match_without_otherwise() {
        let expr = parse(expression(
            vec![rule(
                "r_big",
                cmp("form.amount", "gt", json!(10000)),
                json!([role(&["FINANCE_DIRECTOR"])]),
            )],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": 100 })));
        assert_eq!(outcome, EvalOutcome::NoMatch);
        assert_eq!(outcome.rule_id(), None);
        assert!(
            trace.iter().any(|line| line.contains("无规则命中且无兜底")),
            "trace 必须说明没兜底：{trace:?}"
        );
    }

    /// E-T05 `and` 真短路：`amount=100` 时不去求 `task.priority`（否者会留 Missing）。
    #[test]
    fn e_t05_and_short_circuits() {
        let expr = parse(expression(
            vec![rule(
                "r_and",
                and(vec![
                    cmp("form.amount", "gt", json!(10000)),
                    cmp("task.priority", "eq", json!("P0")),
                ]),
                json!([admin()]),
            )],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": 100 })));
        assert_eq!(outcome, EvalOutcome::NoMatch);
        assert!(
            trace.iter().all(|line| !line.contains("Missing")),
            "and 必须短路：{trace:?}"
        );
    }

    /// E-T06 `or` 短路 + 命中：第一条成立就不求第二条（第二条 Missing）。
    #[test]
    fn e_t06_or_short_circuits() {
        let expr = parse(expression(
            vec![rule(
                "r_or",
                or(vec![
                    cmp("form.amount", "gt", json!(10000)),
                    cmp("task.priority", "eq", json!("P0")),
                ]),
                json!([admin()]),
            )],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": 12000 })));
        assert_eq!(outcome.rule_id(), Some("r_or"));
        assert!(
            trace.iter().all(|line| !line.contains("Missing")),
            "or 必须短路：{trace:?}"
        );
    }

    /// E-T07 `Missing` 视为「不成立」，继续下一条 [trace]。
    #[test]
    fn e_t07_missing_treated_as_false() {
        let expr = parse(expression(
            vec![
                rule(
                    "r1",
                    cmp("form.not_filled", "eq", json!(1)),
                    json!([admin()]),
                ),
                rule(
                    "r2",
                    cmp("form.amount", "gte", json!(1)),
                    json!([role(&["DEPT_MANAGER"])]),
                ),
            ],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": 5 })));
        assert_eq!(outcome.rule_id(), Some("r2"));
        assert!(
            trace
                .iter()
                .any(|line| line
                    .contains("规则 r1 求值失败（Missing: form.not_filled）→ 按不成立处理")),
            "trace 必须点名缺失字段：{trace:?}"
        );
    }

    /// E-T08 `TypeMismatch` 视为「不成立」 [trace]：`amount` 是字符串却用 `gt`。
    #[test]
    fn e_t08_type_mismatch_treated_as_false() {
        let expr = parse(expression(
            vec![rule(
                "r_num",
                cmp("form.amount", "gt", json!(10000)),
                json!([admin()]),
            )],
            None,
        ));
        let (outcome, trace) = trace_of(&expr, &ctx_with(json!({ "amount": "12000" })));
        assert_eq!(outcome, EvalOutcome::NoMatch);
        assert!(
            trace
                .iter()
                .any(|line| line.contains("TypeMismatch: form.amount: 左值不是数字")),
            "trace 必须点名类型不匹配：{trace:?}"
        );
    }

    /// E-T09 `in` / `nin` 集合匹配。
    #[test]
    fn e_t09_in_and_nin() {
        let in_expr = parse(expression(
            vec![rule(
                "r_in",
                cmp("form.type", "in", json!(["硬件", "服务"])),
                json!([admin()]),
            )],
            None,
        ));
        let ctx = ctx_with(json!({ "type": "服务" }));
        assert_eq!(eval_expression(&in_expr, &ctx).rule_id(), Some("r_in"));

        let nin_expr = parse(expression(
            vec![rule(
                "r_nin",
                cmp("form.type", "nin", json!(["硬件", "服务"])),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(eval_expression(&nin_expr, &ctx), EvalOutcome::NoMatch);
    }

    /// E-T10 `is_null` / `not_null`（字段必须显式存在且值为 `null`，否则是 Missing）。
    #[test]
    fn e_t10_is_null_and_not_null() {
        let ctx = ctx_with(json!({ "legal_flag": Value::Null }));
        let is_null = parse(expression(
            vec![rule(
                "r_null",
                unary("form.legal_flag", "is_null"),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(eval_expression(&is_null, &ctx).rule_id(), Some("r_null"));

        let not_null = parse(expression(
            vec![rule(
                "r_not_null",
                unary("form.legal_flag", "not_null"),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(eval_expression(&not_null, &ctx), EvalOutcome::NoMatch);
    }

    /// E-T11 `contains`：字符串含子串 / 数组含元素。
    #[test]
    fn e_t11_contains() {
        let by_text = parse(expression(
            vec![rule(
                "r_text",
                cmp("form.reason", "contains", json!("加急")),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(
            eval_expression(&by_text, &ctx_with(json!({ "reason": "加急采购" }))).rule_id(),
            Some("r_text")
        );

        let by_array = parse(expression(
            vec![rule(
                "r_array",
                cmp("form.tags", "contains", json!("B")),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(
            eval_expression(&by_array, &ctx_with(json!({ "tags": ["A", "B"] }))).rule_id(),
            Some("r_array")
        );
    }

    /// E-T12 `not` 取反。
    #[test]
    fn e_t12_not() {
        let expr = parse(expression(
            vec![rule(
                "r_not",
                not(cmp("form.amount", "gt", json!(10000))),
                json!([admin()]),
            )],
            None,
        ));
        assert_eq!(
            eval_expression(&expr, &ctx_with(json!({ "amount": 100 }))).rule_id(),
            Some("r_not")
        );
        assert_eq!(
            eval_expression(&expr, &ctx_with(json!({ "amount": 20000 }))),
            EvalOutcome::NoMatch
        );
    }

    /// E-T13 `then` 多 target = 并集且**顺序与 JSON 数组一致**（顺序影响去重优先级）。
    #[test]
    fn e_t13_targets_keep_json_order() {
        let expr = parse(expression(
            vec![rule(
                "r_multi",
                always(0),
                json!([role(&["FINANCE_DIRECTOR"]), { "type": "INITIATOR_SUPERIOR", "levels": 2 }]),
            )],
            None,
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 1 })));
        assert_eq!(
            outcome.targets().to_vec(),
            vec![
                Target::Role {
                    role_codes: vec!["FINANCE_DIRECTOR".to_string()]
                },
                Target::InitiatorSuperior { levels: 2 },
            ]
        );

        let resolved = resolve_targets(outcome.targets(), &ResolveContext::default());
        assert_eq!(
            resolved.targets,
            vec![
                ResolvedTarget::RoleCodes(vec!["FINANCE_DIRECTOR".to_string()]),
                ResolvedTarget::Plan(SourcePlan::InitiatorSuperior(2)),
            ]
        );
    }

    /// E-T14 规则 id 重复被拒。
    #[test]
    fn e_t14_duplicate_rule_id_rejected() {
        let reason = parse_err(expression(
            vec![
                rule("r_big", always(0), json!([admin()])),
                rule("r_big", always(1), json!([admin()])),
            ],
            None,
        ));
        assert!(reason.contains("规则 id 重复：r_big"), "{reason}");
    }

    /// E-T15 五个结构性上限逐项超限（全部是结构性，不用计时器）。
    #[test]
    fn e_t15_structural_limits() {
        // ① 17 条规则
        let rules: Vec<Value> = (0..=MAX_EXPR_RULES)
            .map(|index| rule(&format!("r{index}"), always(0), json!([admin()])))
            .collect();
        let reason = parse_err(expression(rules, None));
        assert!(reason.contains("规则数超过 16 条"), "{reason}");

        // ② 单条规则 6 个 target
        let six = json!([admin(), admin(), admin(), admin(), admin(), admin()]);
        let reason = parse_err(expression(vec![rule("r_six", always(0), six)], None));
        assert!(reason.contains("审批人数超过 5 个"), "{reason}");

        // ③ 深度 9 层（graph::parse_condition 的口径：根节点算第 1 层）
        let mut deep = cmp("form.amount", "gt", json!(1));
        for _ in 0..8 {
            deep = and(vec![deep]);
        }
        let reason = parse_err(expression(
            vec![rule("r_deep", deep, json!([admin()]))],
            None,
        ));
        assert!(reason.contains("嵌套层数超过 8 层"), "{reason}");

        // ④ 单条 when 65 节点（上限 64）
        let reason = parse_err(expression(
            vec![rule(
                "r_wide",
                condition_with_nodes(graph::MAX_CONDITION_NODES + 1),
                json!([admin()]),
            )],
            None,
        ));
        assert!(reason.contains("条件节点数超过 64 个"), "{reason}");

        // ⑤ 总 when 节点 257（上限 256）：4 条 64 节点规则合法，第 5 条再加 1 个就超
        let four: Vec<Value> = (0..4)
            .map(|index| {
                rule(
                    &format!("r{index}"),
                    condition_with_nodes(graph::MAX_CONDITION_NODES),
                    json!([admin()]),
                )
            })
            .collect();
        assert!(
            parse_expression(&expression(four.clone(), None)).is_ok(),
            "4 × 64 = 256 不该超限"
        );
        let mut five = four;
        five.push(rule("r_extra", always(0), json!([admin()])));
        let reason = parse_err(expression(five, None));
        assert!(reason.contains("条件节点总数超过 256 个"), "{reason}");
    }

    /// E-T16 保留词与非法目标（4 类文案各自点名）。
    #[test]
    fn e_t16_reserved_and_invalid_targets() {
        let reason = parse_err(expression(
            vec![rule("r1", always(0), json!([{ "type": "PREV_APPROVER" }]))],
            None,
        ));
        assert!(reason.contains("本版本未开放（P2"), "{reason}");

        let reason = parse_err(expression(
            vec![rule("r1", always(0), json!([{ "type": "BOSS" }]))],
            None,
        ));
        assert!(reason.contains("不支持"), "{reason}");

        let reason = parse_err(expression(
            vec![rule("r1", always(0), json!([{ "type": "USER" }]))],
            None,
        ));
        assert!(reason.contains("缺少 user_ids"), "{reason}");

        let reason = parse_err(expression(
            vec![rule(
                "r1",
                always(0),
                json!([{ "type": "ROLE", "role_ids": ["0b6f2c9e-0f8f-4a1a-9b0f-9d0a1f3c2f11"] }]),
            )],
            None,
        ));
        assert!(reason.contains("请用 role_codes"), "{reason}");
    }

    // ---------- E-T17 ~ E-T24：目标解析 / 边界 / 与 finalize 的复用 ----------

    /// E-T17 十种来源各解析一条，并翻译成查询计划（`ROLE` 出角色编码、`DEPT_LEADER` 回落发起人部门）。
    #[test]
    fn e_t17_all_ten_target_sources_map_to_plans() {
        let user_id = "0b6f2c9e-0f8f-4a1a-9b0f-9d0a1f3c2f11";
        let initiator_dept = "11111111-2222-4333-8444-555555555555";
        let expr = parse(expression(
            vec![
                rule(
                    "r_first_five",
                    always(0),
                    json!([
                        { "type": "USER", "user_ids": [user_id] },
                        role(&["FINANCE_DIRECTOR"]),
                        { "type": "DEPT_LEADER" },
                        { "type": "INITIATOR" },
                        { "type": "INITIATOR_SUPERIOR" }
                    ]),
                ),
                rule(
                    "r_last_five",
                    cmp("form.amount", "lt", json!(0)),
                    json!([
                        { "type": "TASK_OWNER" },
                        { "type": "TASK_CREATOR" },
                        { "type": "TASK_ASSIGNER" },
                        { "type": "FORM_VAR", "form_var_key": "approver" },
                        { "type": "ADMIN" }
                    ]),
                ),
            ],
            None,
        ));
        let ctx = ctx_with(json!({ "amount": 1 }));
        let first = eval_expression(&expr, &ctx);
        assert_eq!(first.rule_id(), Some("r_first_five"));
        let parsed_user = Uuid::parse_str(user_id).expect("测试夹具 uuid");
        assert_eq!(
            first.targets().to_vec(),
            vec![
                Target::User {
                    user_ids: vec![parsed_user]
                },
                Target::Role {
                    role_codes: vec!["FINANCE_DIRECTOR".to_string()]
                },
                Target::DeptLeader { dept_ids: vec![] },
                Target::Initiator,
                Target::InitiatorSuperior { levels: 1 },
            ]
        );

        // 第二条规则单独求值（把 amount 改小，让 lt 0 成立）
        let second = eval_expression(&expr, &ctx_with(json!({ "amount": -1 })));
        assert_eq!(second.rule_id(), Some("r_last_five"));
        assert_eq!(
            second.targets().to_vec(),
            vec![
                Target::TaskOwner,
                Target::TaskCreator,
                Target::TaskAssigner,
                Target::FormVar {
                    form_var_key: "approver".to_string()
                },
                Target::Admin,
            ]
        );

        // 翻译：ROLE 出编码（待 assignee.rs 查 code→id）；DEPT_LEADER 空部门回落发起人部门
        let resolve_ctx = ResolveContext {
            initiator_dept_id: Some(Uuid::parse_str(initiator_dept).expect("测试夹具 uuid")),
        };
        let resolved = resolve_targets(first.targets(), &resolve_ctx);
        assert_eq!(
            resolved.targets,
            vec![
                ResolvedTarget::Plan(SourcePlan::Users(vec![parsed_user])),
                ResolvedTarget::RoleCodes(vec!["FINANCE_DIRECTOR".to_string()]),
                ResolvedTarget::Plan(SourcePlan::DeptLeaders(vec![
                    Uuid::parse_str(initiator_dept).expect("测试夹具 uuid")
                ])),
                ResolvedTarget::Plan(SourcePlan::Initiator),
                ResolvedTarget::Plan(SourcePlan::InitiatorSuperior(1)),
            ]
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|note| note.contains("按发起人部门"))
        );
        assert_eq!(resolved.len(), 5);

        let resolved = resolve_targets(second.targets(), &ResolveContext::default());
        assert_eq!(
            resolved.targets,
            vec![
                ResolvedTarget::Plan(SourcePlan::TaskParty(TaskParty::Owner)),
                ResolvedTarget::Plan(SourcePlan::TaskParty(TaskParty::Creator)),
                ResolvedTarget::Plan(SourcePlan::TaskParty(TaskParty::Assigner)),
                ResolvedTarget::Plan(SourcePlan::FormVar("approver".to_string())),
                ResolvedTarget::Plan(SourcePlan::Admins),
            ]
        );

        // 未指定部门且发起人无部门 → 空部门列表（⇒ 该来源 0 人，走 empty_assignee）
        let resolved = resolve_targets(
            &[Target::DeptLeader { dept_ids: vec![] }],
            &ResolveContext::default(),
        );
        assert_eq!(
            resolved.targets,
            vec![ResolvedTarget::Plan(SourcePlan::DeptLeaders(vec![]))]
        );
        assert!(resolved.notes.iter().any(|note| note.contains("解析 0 人")));
    }

    /// E-T18 边界：`then` 空数组 / `rules` 空数组 / `otherwise` 空数组都必须被拒。
    #[test]
    fn e_t18_empty_arrays_rejected() {
        let reason = parse_err(expression(vec![rule("r1", always(0), json!([]))], None));
        assert!(reason.contains("审批人不能为空"), "{reason}");

        let reason = parse_err(expression(vec![], None));
        assert!(reason.contains("至少要有 1 条规则"), "{reason}");

        let reason = parse_err(expression(
            vec![rule("r1", always(0), json!([admin()]))],
            Some(json!([])),
        ));
        assert!(reason.contains("审批人不能为空"), "{reason}");

        let reason = parse_err(expression(
            vec![rule("r1", always(0), json!([admin()]))],
            Some(json!({ "type": "ADMIN" })),
        ));
        assert!(reason.contains("必须是数组"), "{reason}");
    }

    /// E-T19 `when` 的字段必须走白名单（不支持嵌套路径，也不许碰系统表）。
    #[test]
    fn e_t19_when_field_not_in_whitelist() {
        for field in ["form.user.dept", "sys.secret", "biz_task.priority"] {
            let reason = parse_err(expression(
                vec![rule("r1", cmp(field, "eq", json!(1)), json!([admin()]))],
                None,
            ));
            assert!(reason.contains("不在白名单内"), "{field} → {reason}");
        }
        // 允许的形态：form.xxx / 固定字段
        assert!(
            parse_expression(&expression(
                vec![rule(
                    "r1",
                    cmp("task.estimate_hours", "gte", json!(4)),
                    json!([admin()])
                )],
                None,
            ))
            .is_ok()
        );
    }

    /// E-T20 `otherwise` 作为唯一兜底：没有规则成立时它单独生效（单 target）。
    #[test]
    fn e_t20_otherwise_is_the_only_fallback() {
        let expr = parse(expression(
            vec![rule(
                "r_only",
                cmp("form.amount", "gt", json!(10000)),
                json!([role(&["FINANCE_DIRECTOR"])]),
            )],
            Some(json!([admin()])),
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 1 })));
        assert_eq!(
            outcome,
            EvalOutcome::Otherwise {
                targets: vec![Target::Admin],
            }
        );
        assert_eq!(outcome.targets().len(), 1);
    }

    /// E-T21 保存期 / 运行期的「选了 EXPRESSION 却没配规则」必须报错（P1-A2 的 R16 直接用它）。
    #[test]
    fn e_t21_parse_config_without_expression() {
        for config in [
            json!({}),
            json!({ "tip": "财务审批" }),
            json!({ "expression": Value::Null }),
        ] {
            let reason = parse_config(&config).expect_err("没有 expression 必须报错");
            assert!(reason.contains("请先配置规则"), "{config} → {reason}");
        }
    }

    /// E-T22 `config` 里其它键不影响解析；`version` 不认识必须拒绝（数据比代码新 ⇒ 501 的源头）。
    #[test]
    fn e_t22_parse_config_ignores_other_keys_and_rejects_unknown_version() {
        let config = json!({
            "tip": "金额 > 1 万走财务总监",
            "x": 12,
            "expression": {
                "version": 1,
                "rules": [{ "id": "r_big", "when": { "op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10000 }, "then": [role(&["FINANCE_DIRECTOR"])] }]
            }
        });
        let expr = parse_config(&config).expect("config 里其它键不该影响解析");
        assert_eq!(expr.version, EXPRESSION_VERSION);
        assert_eq!(expr.rules.len(), 1);
        assert_eq!(expr.rules[0].id, "r_big");

        let reason = parse_err(json!({
            "version": 2,
            "rules": [{ "id": "r1", "when": { "op": "cmp", "field": "form.amount", "cmp": "gt", "value": 1 }, "then": [admin()] }]
        }));
        assert!(reason.contains("不支持的表达式版本 2"), "{reason}");
    }

    /// E-T23 未知字段必须报错（顶层 / 规则内 / target 内）——静默忽略等于「用户以为配了、其实没配」。
    #[test]
    fn e_t23_unknown_fields_rejected() {
        let mut root = expression(vec![rule("r1", always(0), json!([admin()]))], None);
        root["extra"] = json!(1);
        let reason = parse_err(root);
        assert!(reason.contains("未知字段 extra"), "{reason}");

        let reason = parse_err(expression(
            vec![{
                let mut rule = rule("r1", always(0), json!([admin()]));
                rule["extar"] = json!(true);
                rule
            }],
            None,
        ));
        assert!(reason.contains("未知字段 extar"), "{reason}");

        let reason = parse_err(expression(
            vec![rule(
                "r1",
                always(0),
                json!([{ "type": "ADMIN", "user_ids": ["x"] }]),
            )],
            None,
        ));
        assert!(reason.contains("未知字段 user_ids"), "{reason}");
    }

    /// E-T24 表达式输出接 `finalize()`（复用验证）：多 target 去重 / 自审 SKIP 留痕 / 空审批人三态 / ROBOT 过滤。
    #[test]
    fn e_t24_output_feeds_finalize() {
        let initiator = Uuid::parse_str("0b6f2c9e-0f8f-4a1a-9b0f-9d0a1f3c2f11").expect("夹具");
        let same_person = "22222222-3333-4444-8555-666666666666";
        let same_person_id = Uuid::parse_str(same_person).expect("夹具");

        // ① 多 target 命中同一个人 → finalize 后只剩 1 行待办
        let expr = parse(expression(
            vec![rule(
                "r_union",
                always(0),
                json!([role(&["FINANCE_DIRECTOR"]), { "type": "INITIATOR_SUPERIOR", "levels": 1 }]),
            )],
            None,
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 1 })));
        let plans = resolve_targets(outcome.targets(), &ResolveContext::default());
        // 模拟 assignee.rs 的 load_plan：两个目标都解析到同一个人
        let candidates: Vec<Candidate> = plans
            .targets
            .iter()
            .map(|_| Candidate::new(same_person_id, "张三", AssigneeKind::Role))
            .collect();
        let finalized = finalize(
            candidates,
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(finalized.approvers.len(), 1, "同一人只留 1 行");
        assert!(finalized.notes.iter().any(|note| note.contains("已去重")));

        // ② 表达式含发起人 + self_approval=SKIP → 落 SKIPPED 留痕 + 节点自动通过
        let expr = parse(expression(
            vec![rule("r_self", always(0), json!([{ "type": "INITIATOR" }]))],
            None,
        ));
        let outcome = eval_expression(&expr, &ctx_with(json!({ "amount": 1 })));
        let plans = resolve_targets(outcome.targets(), &ResolveContext::default());
        assert_eq!(
            plans.targets,
            vec![ResolvedTarget::Plan(SourcePlan::Initiator)]
        );
        let finalized = finalize(
            vec![Candidate::new(initiator, "我自己", AssigneeKind::Initiator)],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(finalized.skipped.len(), 1, "自审必须留痕");
        assert!(finalized.auto_pass);

        // ③ 命中但 0 人 + empty_assignee 三态
        let skip = finalize(
            vec![],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(skip.auto_pass);
        assert!(skip.error.is_none());
        let error = finalize(
            vec![],
            SelfPolicy::Skip,
            EmptyPolicy::Error,
            false,
            initiator,
        );
        let api_error = error.error.expect("ERROR 策略必须给业务错误");
        assert_eq!(api_error.code, 65018);
        let fallback = finalize(
            vec![],
            SelfPolicy::Skip,
            EmptyPolicy::FallbackAdmin,
            false,
            initiator,
        );
        assert!(fallback.fallback_admin);

        // ④ ROBOT 过滤：角色来源的机器人被过滤（留痕），显式 USER 来源放行
        let mut robot = Candidate::new(same_person_id, "AI 助手", AssigneeKind::Role);
        robot.is_robot = true;
        let filtered = finalize(
            vec![robot.clone()],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert!(filtered.approvers.is_empty());
        assert!(filtered.notes.iter().any(|note| note.contains("机器人")));
        let mut explicit = robot;
        explicit.source = AssigneeKind::User;
        let kept = finalize(
            vec![explicit],
            SelfPolicy::Skip,
            EmptyPolicy::Skip,
            false,
            initiator,
        );
        assert_eq!(kept.approvers.len(), 1, "显式 USER 的机器人必须放行");
    }
}
