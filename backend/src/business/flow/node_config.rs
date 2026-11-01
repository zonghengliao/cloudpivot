//! `config.task` 解析器（P-FLOW-1A · 设计 §2.3 / §2.7 R18 / §2.10）。
//!
//! # 职责边界（只有三件事）
//!
//! 1. **形状**：`config.task` 必须是一个对象，且**拒绝未知键**（`additionalProperties: false`）。
//!    拼错键名必须当场报错 —— 存进去、跑起来才发现「没生效」是最贵的一类配置缺陷
//!    （与 `expression::deny_unknown_keys` 同一套做法）。
//! 2. **交叉约束**：`due.mode` 与必备键（`RELATIVE`→`amount`+`unit`、`ABSOLUTE`→`date`、
//!    `FIELD`→`field`）、`on_complete=APPROVE` ⇔ `approve_biz_type` 互斥/必备。
//!    违反 → `400 + 65034`（[`error_code::FLOW_NODE_CONFIG_MISMATCH`]）。
//! 3. **组合矩阵**：`need_review` × `on_complete`（R18）→ `400 + 65035`
//!    （[`error_code::FLOW_TASK_NODE_INVALID`]）。
//!
//! **不在这里做的事**（免得两个模块各写一份口径）：
//!
//! - **枚举的取值域**由库侧 `ck_biz_flow_node_task_config` CHECK 兜一遍（V0178），
//!   这里再解析一遍是为了给用户一句中文原因，而不是让数据库抛 500。
//! - **指派来源**不在 `config.task` 里：仍走 `biz_flow_node.assignee_type` 列 +
//!   `biz_flow_node_assignee` 表 + `config.expression`（决策 D3，设计 §2.4）。
//!   本文件**不认识**任何指派字段，免得长成第二套指派规则。
//! - **模板变量白名单**（R21）在 `graph::FlowGraph::validate_with` 里做，因为它要知道
//!   流程的 `biz_type`（决定 `task.*` 是否可用）。本文件只保证模板是字符串。
//!
//! # 为什么配置存 JSONB 而不是物理列（决策 D1）
//!
//! `SnapshotNode` 已带 `config` 字段（`graph.rs`），发布即冻结、`graph_hash` 口径不变。
//! 加物理列等于给 `SnapshotNode` 加 12 个 `Option<…>` ⇒ 所有节点的序列化结果变化 ⇒
//! 存量 4 个 `PUBLISHED` 版本的快照与 hash 全部失真。
//!
//! # 1A 与 1B 的分界
//!
//! `on_complete=APPROVE` 在 1A 的 R18 里**直接拒绝**（引擎没有那条分支）。这不是降级：
//! 静默把 APPROVE 当 DIRECT 跑 = 用户以为配了审批、实际没审 = 内控事故。
//! **P-FLOW-1B 已放开**：任务 `DOING→DONE` 时节点保持 `ACTIVE`，同事务发起
//! `approve_biz_type` 审批单（`task_bridge::on_task_status_changed`），审批通过才推进下游。
//! 唯一保留的 APPROVE 约束是 `APPROVE + need_review=true` 仍然拒绝（R18 的「点两次」）。

use serde_json::{Map, Value};
use std::collections::BTreeMap;

use nik::prelude::*;

use crate::shared::error_code;

/// `title_template` 的字符数上限（与 `biz_task.title varchar(200)` 对齐）。
pub const MAX_TITLE_TEMPLATE_CHARS: usize = 200;
/// `description_template` 的字符数上限（`biz_task.description` 是 text，限 8K 防一次渲染写爆）。
pub const MAX_DESCRIPTION_TEMPLATE_CHARS: usize = 8000;
/// `approve_biz_type` 的长度上限（`biz_flow_def.biz_type varchar(32)`）。
pub const MAX_APPROVE_BIZ_TYPE_CHARS: usize = 32;
/// `due.amount` 的上限（10 年，再长就不该用流程推了）。
pub const MAX_DUE_AMOUNT: i64 = 3650;
/// `due.field` / `form_mapping` 值的路径长度上限（与 `field_allowed` 的 64 + `根.` 对齐）。
pub const MAX_FIELD_PATH_CHARS: usize = 72;

/// `config.task` 的**全部合法键**（`additionalProperties: false` 的唯一依据）。
pub const TASK_CONFIG_KEYS: [&str; 12] = [
    "title_template",
    "description_template",
    "priority",
    "due",
    "need_review",
    "on_complete",
    "approve_biz_type",
    "on_reject",
    "on_cancel",
    "form_mapping",
    "attach_biz",
    "allow_skip_when_no_assignee",
];

/// `due` 的合法键。
const DUE_KEYS: [&str; 5] = ["mode", "amount", "unit", "date", "field"];

/// `form_mapping` 的合法键（**只有这三个**；title / owner / status 不许映射）。
pub const FORM_MAPPING_KEYS: [&str; 3] = ["priority", "due_date", "estimate_hours"];

// ==================== 枚举 ====================

/// 任务完成后流程怎么走（`config.task.on_complete`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnComplete {
    /// 干完即算通过（负责人 `DOING→DONE`）。
    Direct,
    /// 任务进 `REVIEW`，由 creator（= 流程发起人）验收后通过。
    Verify,
    /// 完成后自动发起一个审批流（**P-FLOW-1B 落地**：任务 `DOING→DONE` 时节点保持 `ACTIVE`，
    /// 同事务发起 `approve_biz_type` 审批单，审批通过才推进下游）。
    Approve,
}

impl OnComplete {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnComplete::Direct => "DIRECT",
            OnComplete::Verify => "VERIFY",
            OnComplete::Approve => "APPROVE",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "DIRECT" => Some(OnComplete::Direct),
            "VERIFY" => Some(OnComplete::Verify),
            "APPROVE" => Some(OnComplete::Approve),
            _ => None,
        }
    }
}

/// 任务被「打回」（`REVIEW→DOING`）时流程怎么走（`config.task.on_reject`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnReject {
    /// 退回上一节点（复用引擎 `apply_return`，`round+1`）。
    BackToPrev,
    /// 终止实例（`REJECTED`）。
    Terminate,
    /// 不动，等任务再完成（默认；「返工」本来就该等）。
    Stay,
}

impl OnReject {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnReject::BackToPrev => "BACK_TO_PREV",
            OnReject::Terminate => "TERMINATE",
            OnReject::Stay => "STAY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "BACK_TO_PREV" => Some(OnReject::BackToPrev),
            "TERMINATE" => Some(OnReject::Terminate),
            "STAY" => Some(OnReject::Stay),
            _ => None,
        }
    }
}

/// 任务被取消（`CANCELLED`）时流程怎么走（`config.task.on_cancel`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnCancel {
    /// 终止实例（默认：活被取消了，流程不该继续往下走）。
    Terminate,
    /// 退回上一节点。
    BackToPrev,
    /// 不动。
    Stay,
}

impl OnCancel {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnCancel::Terminate => "TERMINATE",
            OnCancel::BackToPrev => "BACK_TO_PREV",
            OnCancel::Stay => "STAY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "TERMINATE" => Some(OnCancel::Terminate),
            "BACK_TO_PREV" => Some(OnCancel::BackToPrev),
            "STAY" => Some(OnCancel::Stay),
            _ => None,
        }
    }
}

/// 期限模式（`config.task.due.mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueMode {
    /// 从节点激活时刻起算。
    Relative,
    /// 固定日期。
    Absolute,
    /// 从表单字段取日期。
    Field,
}

impl DueMode {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            DueMode::Relative => "RELATIVE",
            DueMode::Absolute => "ABSOLUTE",
            DueMode::Field => "FIELD",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "RELATIVE" => Some(DueMode::Relative),
            "ABSOLUTE" => Some(DueMode::Absolute),
            "FIELD" => Some(DueMode::Field),
            _ => None,
        }
    }
}

/// 相对期限的单位（`config.task.due.unit`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueUnit {
    /// 小时。
    Hour,
    /// 自然日。
    Day,
    /// 工作日（跳过周六周日；本仓库暂无节假日表，见设计 §8.4）。
    Workday,
}

impl DueUnit {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            DueUnit::Hour => "HOUR",
            DueUnit::Day => "DAY",
            DueUnit::Workday => "WORKDAY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "HOUR" => Some(DueUnit::Hour),
            "DAY" => Some(DueUnit::Day),
            "WORKDAY" => Some(DueUnit::Workday),
            _ => None,
        }
    }
}

/// 任务优先级取值域（`biz_task.priority varchar(2)` + 4 值 CHECK，**固定枚举不做模板**）。
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
/// 默认优先级。
pub const DEFAULT_PRIORITY: &str = "P2";

// ==================== 配置结构 ====================

/// 期限配置（`config.task.due`）。三个模式互斥，`parse` 已保证只填对应模式的键。
#[derive(Debug, Clone, PartialEq)]
pub struct DueConfig {
    /// 模式。
    pub mode: DueMode,
    /// 仅 `RELATIVE`：数量。
    pub amount: Option<i64>,
    /// 仅 `RELATIVE`：单位。
    pub unit: Option<DueUnit>,
    /// 仅 `ABSOLUTE`：`yyyy-MM-dd`。
    pub date: Option<String>,
    /// 仅 `FIELD`：表单字段路径（必须 `form.*`）。
    pub field: Option<String>,
}

/// `config.task` 的强类型形态（**解析成功后引擎只读它**，不再回读 JSON）。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskNodeConfig {
    /// 任务标题模板。
    pub title_template: String,
    /// 任务描述模板（可空）。
    pub description_template: Option<String>,
    /// 优先级（`P0`–`P3`）。
    pub priority: String,
    /// 期限（可空 = 无期限）。
    pub due: Option<DueConfig>,
    /// 是否需验收。
    pub need_review: bool,
    /// 完成后怎么走。
    pub on_complete: OnComplete,
    /// 仅 `APPROVE`：审批流 `biz_type`。
    pub approve_biz_type: Option<String>,
    /// 打回策略。
    pub on_reject: OnReject,
    /// 取消策略。
    pub on_cancel: OnCancel,
    /// 任务字段 ← 表单字段映射（`priority` / `due_date` / `estimate_hours`）。
    pub form_mapping: BTreeMap<String, String>,
    /// 任务详情页是否显示「所属业务单据」卡片。
    pub attach_biz: bool,
    /// `empty_assignee=SKIP` 的额外确认开关（R20；默认 false = 不许静默丢活）。
    pub allow_skip_when_no_assignee: bool,
}

impl Default for TaskNodeConfig {
    fn default() -> Self {
        Self {
            title_template: String::new(),
            description_template: None,
            priority: DEFAULT_PRIORITY.to_string(),
            due: None,
            need_review: false,
            on_complete: OnComplete::Direct,
            approve_biz_type: None,
            on_reject: OnReject::BackToPrev,
            on_cancel: OnCancel::Terminate,
            form_mapping: BTreeMap::new(),
            attach_biz: true,
            allow_skip_when_no_assignee: false,
        }
    }
}

// ==================== 错误构造 ====================

/// `400 + 65034`：配置形状 / 未知键 / 交叉约束不合法。
pub fn config_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        400,
        error_code::FLOW_NODE_CONFIG_MISMATCH,
        format!("任务配置非法：{}", message.into()),
    )
}

/// `400 + 65035`：`need_review` × `on_complete` 组合非法（R18）。
pub fn matrix_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        400,
        error_code::FLOW_TASK_NODE_INVALID,
        format!("配置矛盾：{}", message.into()),
    )
}

// ==================== 解析 ====================

/// 节点配置里的 `config.task` 对象（`None` = 没配）。
pub fn task_config_of(config: &Value) -> Option<&Value> {
    config.get("task")
}

/// 解析 `config.task`（**形状 + 交叉约束**，不含 R18 矩阵与 R21 模板白名单）。
///
/// # 参数
///
/// - `value`：`biz_flow_node.config.task` 的值（调用方从 JSONB / 请求体里取出来）。
///
/// # 返回值
///
/// 强类型 [`TaskNodeConfig`]（缺省键已填默认值）。
///
/// # 异常情况
///
/// 全部 `400 + 65034`，文案直接面向配置管理员：
///
/// - 不是对象 / `title_template` 缺失或超长 / 枚举取值非法；
/// - **未知键**（拼错的键名）；
/// - `due.mode` 与必备键不匹配、或带上了别的模式的键；
/// - `approve_biz_type` 与 `on_complete` 不互斥（非 APPROVE 却带 / APPROVE 却没带）；
/// - `form_mapping` 的键不在三个白名单内、值不是 `form.*` 路径。
pub fn parse_task_config(value: &Value) -> std::result::Result<TaskNodeConfig, ApiError> {
    let Some(object) = value.as_object() else {
        return Err(config_invalid("config.task 必须是对象"));
    };
    deny_unknown_keys(object, &TASK_CONFIG_KEYS, "config.task")?;

    let title_template = required_string(object, "title_template")?;
    let title_chars = title_template.chars().count();
    if title_chars == 0 {
        return Err(config_invalid("title_template 不能为空"));
    }
    if title_chars > MAX_TITLE_TEMPLATE_CHARS {
        return Err(config_invalid(format!(
            "title_template 有 {title_chars} 个字符，超过上限 {MAX_TITLE_TEMPLATE_CHARS}（biz_task.title 是 varchar(200)）"
        )));
    }

    let description_template = match optional_string(object, "description_template")? {
        Some(text) => {
            let chars = text.chars().count();
            if chars > MAX_DESCRIPTION_TEMPLATE_CHARS {
                return Err(config_invalid(format!(
                    "description_template 有 {chars} 个字符，超过上限 {MAX_DESCRIPTION_TEMPLATE_CHARS}"
                )));
            }
            Some(text)
        }
        None => None,
    };

    let priority = match optional_string(object, "priority")? {
        Some(raw) => {
            let upper = raw.to_ascii_uppercase();
            if !PRIORITIES.contains(&upper.as_str()) {
                return Err(config_invalid(format!(
                    "priority 取值非法：{raw}（应为 {}）",
                    PRIORITIES.join(" / ")
                )));
            }
            upper
        }
        None => DEFAULT_PRIORITY.to_string(),
    };

    let due = match object.get("due") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_due(value)?),
    };

    let need_review = optional_bool(object, "need_review")?.unwrap_or(false);

    let on_complete_raw = required_string(object, "on_complete")?;
    let on_complete = OnComplete::parse(&on_complete_raw).ok_or_else(|| {
        config_invalid(format!(
            "on_complete 取值非法：{on_complete_raw}（应为 DIRECT / VERIFY / APPROVE）"
        ))
    })?;

    let approve_raw = optional_string(object, "approve_biz_type")?;
    if on_complete == OnComplete::Approve && approve_raw.is_none() {
        return Err(config_invalid(
            "on_complete=APPROVE 必须指定 approve_biz_type",
        ));
    }
    if on_complete != OnComplete::Approve && approve_raw.is_some() {
        return Err(config_invalid(format!(
            "approve_biz_type 只在 on_complete=APPROVE 时可填（当前 on_complete={}）",
            on_complete.as_str()
        )));
    }
    let approve_biz_type = match approve_raw {
        Some(raw) => {
            if raw.chars().count() > MAX_APPROVE_BIZ_TYPE_CHARS {
                return Err(config_invalid(format!(
                    "approve_biz_type 超过 {MAX_APPROVE_BIZ_TYPE_CHARS} 个字符"
                )));
            }
            if !is_biz_type_literal(&raw) {
                return Err(config_invalid(format!(
                    "approve_biz_type 格式非法：{raw}（应为大写字母开头的大写字母 / 数字 / 下划线，如 TASK_ACCEPT）"
                )));
            }
            Some(raw)
        }
        None => None,
    };

    let on_reject = match optional_string(object, "on_reject")? {
        Some(raw) => OnReject::parse(&raw).ok_or_else(|| {
            config_invalid(format!(
                "on_reject 取值非法：{raw}（应为 BACK_TO_PREV / TERMINATE / STAY）"
            ))
        })?,
        None => OnReject::BackToPrev,
    };

    let on_cancel = match optional_string(object, "on_cancel")? {
        Some(raw) => OnCancel::parse(&raw).ok_or_else(|| {
            config_invalid(format!(
                "on_cancel 取值非法：{raw}（应为 TERMINATE / BACK_TO_PREV / STAY）"
            ))
        })?,
        None => OnCancel::Terminate,
    };

    let form_mapping = parse_form_mapping(object)?;
    let attach_biz = optional_bool(object, "attach_biz")?.unwrap_or(true);
    let allow_skip_when_no_assignee =
        optional_bool(object, "allow_skip_when_no_assignee")?.unwrap_or(false);

    Ok(TaskNodeConfig {
        title_template,
        description_template,
        priority,
        due,
        need_review,
        on_complete,
        approve_biz_type,
        on_reject,
        on_cancel,
        form_mapping,
        attach_biz,
        allow_skip_when_no_assignee,
    })
}

/// R18 组合矩阵（`need_review` × `on_complete`）+ 1A 阶段闸。
///
/// # 判定表（设计 §2.3，逐行落地）
///
/// | `on_complete` | `need_review` | 结果 |
/// |---|---|---|
/// | `DIRECT` | `false` | ✅ 推荐（干完即通过） |
/// | `DIRECT` | `true` | ✅ 允许（干完 → REVIEW → 发起人验收 → 通过） |
/// | `VERIFY` | `true` | ✅ 允许（= 上一行，`VERIFY` 是显式声明） |
/// | `VERIFY` | `false` | ❌ 矛盾：声明要验收却没开验收态（配置静默失效） |
/// | `APPROVE` | `true` | ❌「点两次提交」（验收一次 + 审批再签一次） |
/// | `APPROVE` | `false` | ✅（P-FLOW-1B 起；任务 `DOING→DONE` 时同事务发起审批单） |
///
/// # 异常情况
///
/// `400 + 65035`。
pub fn check_matrix(cfg: &TaskNodeConfig) -> std::result::Result<(), ApiError> {
    match (cfg.on_complete, cfg.need_review) {
        (OnComplete::Verify, false) => Err(matrix_invalid(
            "on_complete=VERIFY 必须同时开 need_review（否则声明要验收，却允许负责人自己点完成直接通过）",
        )),
        (OnComplete::Approve, true) => Err(matrix_invalid(
            "on_complete=APPROVE 与 need_review=true 不能同时开启（会让用户点两次：发起人验收 + 审批人签字）",
        )),
        // `APPROVE + need_review=false` 从 P-FLOW-1B 起**放行**：任务 `DOING→DONE` 时
        // 节点保持 `ACTIVE` 并同事务发起 `approve_biz_type` 审批单，审批通过才推进下游。
        // 1A 的那条阶段闸（直接拒绝）已随引擎分支落地而删除。
        _ => Ok(()),
    }
}

/// `due.mode` 与必备键的交叉约束（`400 + 65034`）。
fn parse_due(value: &Value) -> std::result::Result<DueConfig, ApiError> {
    let Some(object) = value.as_object() else {
        return Err(config_invalid("due 必须是对象"));
    };
    deny_unknown_keys(object, &DUE_KEYS, "config.task.due")?;

    let mode_raw = required_string(object, "mode")
        .map_err(|_| config_invalid("due.mode 必填（RELATIVE / ABSOLUTE / FIELD）"))?;
    let mode = DueMode::parse(&mode_raw).ok_or_else(|| {
        config_invalid(format!(
            "due.mode 取值非法：{mode_raw}（应为 RELATIVE / ABSOLUTE / FIELD）"
        ))
    })?;

    let amount = optional_i64(object, "amount")?;
    let unit = match optional_string(object, "unit")? {
        Some(raw) => Some(DueUnit::parse(&raw).ok_or_else(|| {
            config_invalid(format!(
                "due.unit 取值非法：{raw}（应为 HOUR / DAY / WORKDAY）"
            ))
        })?),
        None => None,
    };
    let date = optional_string(object, "date")?;
    let field = optional_string(object, "field")?;

    match mode {
        DueMode::Relative => {
            let amount = amount.ok_or_else(|| {
                config_invalid("due.mode=RELATIVE 必须指定 amount（数量）与 unit（单位）")
            })?;
            if !(1..=MAX_DUE_AMOUNT).contains(&amount) {
                return Err(config_invalid(format!(
                    "due.amount 必须在 1..{MAX_DUE_AMOUNT} 之间（当前 {amount}）"
                )));
            }
            let unit = unit.ok_or_else(|| {
                config_invalid("due.mode=RELATIVE 必须指定 amount（数量）与 unit（单位）")
            })?;
            reject_keys_for_mode(
                &[("date", date.is_some()), ("field", field.is_some())],
                mode,
            )?;
            Ok(DueConfig {
                mode,
                amount: Some(amount),
                unit: Some(unit),
                date: None,
                field: None,
            })
        }
        DueMode::Absolute => {
            let date = date
                .ok_or_else(|| config_invalid("due.mode=ABSOLUTE 必须指定 date（yyyy-MM-dd）"))?;
            if !is_iso_date(&date) {
                return Err(config_invalid(format!(
                    "due.date 格式非法：{date}（应为 yyyy-MM-dd）"
                )));
            }
            reject_keys_for_mode(
                &[
                    ("amount", amount.is_some()),
                    ("unit", unit.is_some()),
                    ("field", field.is_some()),
                ],
                mode,
            )?;
            Ok(DueConfig {
                mode,
                amount: None,
                unit: None,
                date: Some(date),
                field: None,
            })
        }
        DueMode::Field => {
            let field = field.ok_or_else(|| {
                config_invalid("due.mode=FIELD 必须指定 field（表单日期字段路径）")
            })?;
            if field.chars().count() > MAX_FIELD_PATH_CHARS {
                return Err(config_invalid(format!(
                    "due.field 超过 {MAX_FIELD_PATH_CHARS} 个字符"
                )));
            }
            reject_keys_for_mode(
                &[
                    ("amount", amount.is_some()),
                    ("unit", unit.is_some()),
                    ("date", date.is_some()),
                ],
                mode,
            )?;
            Ok(DueConfig {
                mode,
                amount: None,
                unit: None,
                date: None,
                field: Some(field),
            })
        }
    }
}

/// 某个模式下不允许出现的键（配置了就是「两个模式同时声明」）。
fn reject_keys_for_mode(
    present: &[(&str, bool)],
    mode: DueMode,
) -> std::result::Result<(), ApiError> {
    for (key, has) in present {
        if *has {
            return Err(config_invalid(format!(
                "due.mode={} 不允许出现 {key}（三个模式互斥）",
                mode.as_str()
            )));
        }
    }
    Ok(())
}

/// `form_mapping`：只有三个键，值必须是 `form.*` 路径。
fn parse_form_mapping(
    object: &Map<String, Value>,
) -> std::result::Result<BTreeMap<String, String>, ApiError> {
    let mut out = BTreeMap::new();
    let Some(value) = object.get("form_mapping").filter(|v| !v.is_null()) else {
        return Ok(out);
    };
    let Some(map) = value.as_object() else {
        return Err(config_invalid("form_mapping 必须是对象"));
    };
    for (key, raw) in map {
        if !FORM_MAPPING_KEYS.contains(&key.as_str()) {
            return Err(config_invalid(format!(
                "form_mapping 不允许映射字段 {key}（只允许 {}：title / owner / status 必须由模板与指派规则决定，否则两套口径）",
                FORM_MAPPING_KEYS.join(" / ")
            )));
        }
        let Some(path) = raw.as_str() else {
            return Err(config_invalid(format!(
                "form_mapping.{key} 必须是字符串（表单字段路径）"
            )));
        };
        let path = path.trim();
        if path.chars().count() > MAX_FIELD_PATH_CHARS {
            return Err(config_invalid(format!(
                "form_mapping.{key} 超过 {MAX_FIELD_PATH_CHARS} 个字符"
            )));
        }
        if !path.starts_with("form.") {
            return Err(config_invalid(format!(
                "form_mapping.{key} 必须是 form.* 路径（当前 {path}）"
            )));
        }
        out.insert(key.clone(), path.to_string());
    }
    Ok(out)
}

// ==================== 小工具 ====================

/// 拒绝未知键（`additionalProperties: false`）。
fn deny_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    scope: &str,
) -> std::result::Result<(), ApiError> {
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(config_invalid(format!(
        "{scope} 出现未知键：{}（允许：{}）",
        unknown.join(" / "),
        allowed.join(" / ")
    )))
}

/// 必填字符串（缺失 / 非字符串 / 空白 → 65034）。
fn required_string(
    object: &Map<String, Value>,
    key: &str,
) -> std::result::Result<String, ApiError> {
    optional_string(object, key)?.ok_or_else(|| config_invalid(format!("{key} 必填")))
}

/// 可选字符串（缺失 / `null` → `None`；非字符串 / 空白 → 65034）。
fn optional_string(
    object: &Map<String, Value>,
    key: &str,
) -> std::result::Result<Option<String>, ApiError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(config_invalid(format!("{key} 不能是空字符串")));
            }
            Ok(Some(trimmed.to_string()))
        }
        Some(_) => Err(config_invalid(format!("{key} 必须是字符串"))),
    }
}

/// 可选布尔。
fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
) -> std::result::Result<Option<bool>, ApiError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(config_invalid(format!("{key} 必须是布尔值"))),
    }
}

/// 可选整数。
fn optional_i64(
    object: &Map<String, Value>,
    key: &str,
) -> std::result::Result<Option<i64>, ApiError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| config_invalid(format!("{key} 必须是整数"))),
        Some(_) => Err(config_invalid(format!("{key} 必须是整数"))),
    }
}

/// `biz_type` 字面量：`^[A-Z][A-Z0-9_]{0,31}$`。
fn is_biz_type_literal(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    raw.chars().count() <= MAX_APPROVE_BIZ_TYPE_CHARS
        && chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

/// `yyyy-MM-dd` 语法判定（只判形状；「不得早于今天」是运行期口径，见设计 §8.4）。
fn is_iso_date(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({
            "title_template": "{{instance.biz_no}} 催货",
            "on_complete": "DIRECT"
        })
    }

    /// 正常配置：缺省值逐条落地（priority=P2 / on_reject=BACK_TO_PREV / on_cancel=TERMINATE）。
    #[test]
    fn parse_minimal_uses_documented_defaults() {
        let cfg = parse_task_config(&base()).expect("最小配置必须能解析");
        assert_eq!(cfg.title_template, "{{instance.biz_no}} 催货");
        assert_eq!(cfg.priority, "P2");
        assert_eq!(cfg.on_complete, OnComplete::Direct);
        assert_eq!(cfg.on_reject, OnReject::BackToPrev);
        assert_eq!(cfg.on_cancel, OnCancel::Terminate);
        assert!(!cfg.need_review);
        assert!(cfg.attach_biz, "attach_biz 默认 true");
        assert!(!cfg.allow_skip_when_no_assignee);
        assert!(cfg.due.is_none());
        assert!(cfg.form_mapping.is_empty());
    }

    /// 交叉约束 ①：APPROVE 缺 biz_type → 65034。
    #[test]
    fn parse_rejects_approve_without_biz_type() {
        let mut raw = base();
        raw["on_complete"] = json!("APPROVE");
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert_eq!(error.status, 400);
        assert!(
            error.msg.contains("approve_biz_type"),
            "文案要点名缺什么：{}",
            error.msg
        );
    }

    /// 交叉约束 ②：非 APPROVE 却带 biz_type（互斥键）→ 65034。
    #[test]
    fn parse_rejects_biz_type_without_approve() {
        let mut raw = base();
        raw["approve_biz_type"] = json!("TASK_ACCEPT");
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
    }

    /// 交叉约束 ③：RELATIVE 缺 amount → 65034；ABSOLUTE 缺 date → 65034；FIELD 缺 field → 65034。
    #[test]
    fn parse_rejects_due_mode_missing_keys() {
        for (due, expect) in [
            (json!({"mode": "RELATIVE", "unit": "DAY"}), "amount"),
            (json!({"mode": "RELATIVE", "amount": 3}), "amount"),
            (json!({"mode": "ABSOLUTE"}), "date"),
            (json!({"mode": "FIELD"}), "field"),
        ] {
            let mut raw = base();
            raw["due"] = due.clone();
            let error = parse_task_config(&raw).unwrap_err();
            assert_eq!(
                error.code,
                error_code::FLOW_NODE_CONFIG_MISMATCH,
                "due={due} 应被拒"
            );
            assert!(
                error.msg.contains(expect),
                "due={due} 的文案应点名 {expect}：{}",
                error.msg
            );
        }
    }

    /// 交叉约束 ④：未知键（含 `due` 里的未知键）→ 65034，且文案列出来。
    #[test]
    fn parse_rejects_unknown_keys() {
        let mut raw = base();
        raw["titel_template"] = json!("拼错键名");
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(error.msg.contains("titel_template"));

        let mut raw = base();
        raw["due"] = json!({"mode": "RELATIVE", "amount": 2, "unit": "DAY", "amount2": 3});
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(error.msg.contains("amount2"));
    }

    /// 交叉约束 ⑤：模式互斥键（RELATIVE 带 date / ABSOLUTE 带 amount）→ 65034。
    #[test]
    fn parse_rejects_due_mode_conflict() {
        for due in [
            json!({"mode": "RELATIVE", "amount": 2, "unit": "DAY", "date": "2026-11-15"}),
            json!({"mode": "ABSOLUTE", "date": "2026-11-15", "amount": 2}),
            json!({"mode": "FIELD", "field": "form.due", "unit": "DAY"}),
        ] {
            let mut raw = base();
            raw["due"] = due.clone();
            let error = parse_task_config(&raw).unwrap_err();
            assert_eq!(
                error.code,
                error_code::FLOW_NODE_CONFIG_MISMATCH,
                "due={due} 应被拒"
            );
        }
    }

    /// 交叉约束 ⑥：`form_mapping` 只能映射三个键，值必须是 `form.*`。
    #[test]
    fn parse_rejects_bad_form_mapping() {
        let mut raw = base();
        raw["form_mapping"] = json!({"title": "form.title"});
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);

        let mut raw = base();
        raw["form_mapping"] = json!({"priority": "instance.priority"});
        let error = parse_task_config(&raw).unwrap_err();
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);

        let mut ok = base();
        ok["form_mapping"] = json!({"priority": "form.urgency", "due_date": "form.delivery_date"});
        let cfg = parse_task_config(&ok).expect("合法映射应通过");
        assert_eq!(cfg.form_mapping.len(), 2);
        assert_eq!(cfg.form_mapping["due_date"], "form.delivery_date");
    }

    /// R18 矩阵：4 个允许 / 拒绝组合逐条断言。
    #[test]
    fn matrix_follows_design_table() {
        let mut cfg = TaskNodeConfig {
            title_template: "t".to_string(),
            ..TaskNodeConfig::default()
        };
        // DIRECT × false / true 都允许
        assert!(check_matrix(&cfg).is_ok());
        cfg.need_review = true;
        assert!(check_matrix(&cfg).is_ok());
        // VERIFY × true 允许，× false 拒绝
        cfg.on_complete = OnComplete::Verify;
        assert!(check_matrix(&cfg).is_ok());
        cfg.need_review = false;
        assert_eq!(
            check_matrix(&cfg).unwrap_err().code,
            error_code::FLOW_TASK_NODE_INVALID
        );
        // APPROVE × true 拒绝（点两次）；× false 从 P-FLOW-1B 起**允许**（引擎分支已落地）
        cfg.on_complete = OnComplete::Approve;
        cfg.need_review = true;
        assert_eq!(
            check_matrix(&cfg).unwrap_err().code,
            error_code::FLOW_TASK_NODE_INVALID
        );
        cfg.need_review = false;
        assert!(
            check_matrix(&cfg).is_ok(),
            "P-FLOW-1B 起 APPROVE × need_review=false 必须放行（引擎已有「完成即发起审批」分支）"
        );
    }

    /// 枚举往返：`as_str` / `parse` 逐值互逆（大小写不敏感）。
    #[test]
    fn enums_round_trip() {
        for value in [OnComplete::Direct, OnComplete::Verify, OnComplete::Approve] {
            assert_eq!(OnComplete::parse(value.as_str()), Some(value));
            assert_eq!(
                OnComplete::parse(&value.as_str().to_ascii_lowercase()),
                Some(value)
            );
        }
        for value in [OnReject::BackToPrev, OnReject::Terminate, OnReject::Stay] {
            assert_eq!(OnReject::parse(value.as_str()), Some(value));
        }
        for value in [OnCancel::Terminate, OnCancel::BackToPrev, OnCancel::Stay] {
            assert_eq!(OnCancel::parse(value.as_str()), Some(value));
        }
        for value in [DueMode::Relative, DueMode::Absolute, DueMode::Field] {
            assert_eq!(DueMode::parse(value.as_str()), Some(value));
        }
        for value in [DueUnit::Hour, DueUnit::Day, DueUnit::Workday] {
            assert_eq!(DueUnit::parse(value.as_str()), Some(value));
        }
        assert_eq!(OnComplete::parse("APPROVED"), None);
    }
}
