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
use nik::uuid::Uuid;

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

// ==================== P-FLOW-2：`config.notify` 解析器 ====================
//
// 与 `config.task` 同一套纪律（形状 + 交叉约束 + 拒绝未知键），三处刻意不同：
//
// 1. **没有"矩阵"**：通知节点不产生阻塞语义，`when` 三值互不组合，不存在
//    `need_review × on_complete` 那类交叉禁忌；
// 2. **收件人允许为空集**：`targets` 解析出 0 人时按 `skip_if_empty` 处理（默认 WARN），
//    而不是像审批节点那样 65018 —— 通知发不出去不影响流程推进（见 `notify_node` 头注释）；
// 3. **模板二选一由库侧 CHECK + 这里双保险**：库抛的是 500，这里给的是中文 400。

/// `config.notify.targets` 的条数上限（V0178 的 `jsonb_array_length BETWEEN 1 AND 5`）。
pub const MAX_NOTIFY_TARGETS: usize = 5;
/// `template_code` 长度上限（`sys_notice_template.template_code varchar(64)`）。
pub const MAX_NOTIFY_TEMPLATE_CODE_CHARS: usize = 64;
/// 内联标题上限（`sys_notice.title varchar(255)`，留 55 字余量给渲染后的变量）。
pub const MAX_NOTIFY_TITLE_CHARS: usize = 200;
/// 内联正文上限（一次渲染最多写 4K，防模板里塞整篇文档）。
pub const MAX_NOTIFY_CONTENT_CHARS: usize = 4000;
/// 单个 target 里 `user_ids` / `dept_ids` 的条数上限。
pub const MAX_NOTIFY_TARGET_IDS: usize = 20;
/// 单个 target 里 `role_codes` 的条数上限。
pub const MAX_NOTIFY_ROLE_CODES: usize = 10;

/// `config.notify` 的**全部合法键**（`additionalProperties: false` 的唯一依据）。
///
/// 顺序与设计文档 §2.6 的 JSON Schema 一致；新增键必须同时改这里与前端 `NotifyConfig`。
pub const NOTIFY_CONFIG_KEYS: [&str; 9] = [
    "when",
    "targets",
    "prev_approver",
    "template_code",
    "inline",
    "channels",
    "level",
    "force",
    "skip_if_empty",
];

/// `inline` 的合法键。
const INLINE_KEYS: [&str; 3] = ["title", "content", "content_format"];

/// `targets[]` 的合法键。
const NOTIFY_TARGET_KEYS: [&str; 6] = [
    "type",
    "user_ids",
    "role_codes",
    "dept_ids",
    "levels",
    "form_var_key",
];

/// 通知时机（`config.notify.when`）。
///
/// # 三种时机的真实差别（P-FLOW-2 的引擎口径）
///
/// | 值 | 引擎行为 | 观察差异 |
/// |----|----------|----------|
/// | `ON_ENTER` | 进入即渲染 + 产生 effect，**然后**节点 `APPROVED`，再驱动下游 | effect 编号段 `ENTER` |
/// | `ON_LEAVE` | 节点**先**`APPROVED`，**再**产生 effect，然后驱动下游 | effect 编号段 `LEAVE` |
/// | `ON_TIMEOUT` | 节点保持 `ACTIVE`（不产生 effect），`timeout_hours` 后由定时扫描调用 `engine::fire_notify_node` 发送并放行 | 节点在超时前一直是「进行中」 |
///
/// `ON_ENTER` 与 `ON_LEAVE` 的投递时刻相同（都在事务提交后，见 `engine::flush_drafts`）——
/// 非阻塞节点没有「停留期」，两者唯一的差别是**通知时点相对节点通过**的先后。
/// 保留两个值是因为设计文档 §2.6 明确定义了它们（前端下拉框已经这么画），
/// 而不是为了制造语义幻觉；这一点在 `notify_node` 头注释里也写了一遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyWhen {
    /// 节点被激活时发。
    OnEnter,
    /// 节点结束时发。
    OnLeave,
    /// 激活后 `timeout_hours` 仍未离开时发。
    OnTimeout,
}

impl NotifyWhen {
    /// 入库 / 出参字符串（与 V0178 的 CHECK 逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyWhen::OnEnter => "ON_ENTER",
            NotifyWhen::OnLeave => "ON_LEAVE",
            NotifyWhen::OnTimeout => "ON_TIMEOUT",
        }
    }

    /// 全部取值（错误文案「应为 …」用）。
    pub const ALL: [NotifyWhen; 3] = [
        NotifyWhen::OnEnter,
        NotifyWhen::OnLeave,
        NotifyWhen::OnTimeout,
    ];

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ON_ENTER" => Some(NotifyWhen::OnEnter),
            "ON_LEAVE" => Some(NotifyWhen::OnLeave),
            "ON_TIMEOUT" => Some(NotifyWhen::OnTimeout),
            _ => None,
        }
    }

    /// 是否需要在节点上「等」一段时间（只有 `ON_TIMEOUT` 会阻塞）。
    pub fn is_deferred(self) -> bool {
        matches!(self, NotifyWhen::OnTimeout)
    }

    /// 幂等键里的时机段（同一实例/节点/轮次下，两个时机不会互相吞掉）。
    pub fn code(self) -> &'static str {
        match self {
            NotifyWhen::OnEnter => "ENTER",
            NotifyWhen::OnLeave => "LEAVE",
            NotifyWhen::OnTimeout => "TIMEOUT",
        }
    }

    /// 幂等键里的**单字母**时机段（`code()` 太长，`sys_notice.idempotency_key` 只有 128 位）。
    pub fn idempotency_segment(self) -> &'static str {
        match self {
            NotifyWhen::OnEnter => "E",
            NotifyWhen::OnLeave => "L",
            NotifyWhen::OnTimeout => "T",
        }
    }
}

/// 通知级别（`config.notify.level`）。
///
/// # 与库侧 `ck_sys_notice_level` 的差异
///
/// 设计文档 §2.6 写的是 `INFO / WARN / URGENT`，而 `sys_notice.level` 的 CHECK 是
/// `INFO / SUCCESS / WARN / ERROR`（V0043）。两边都不改的前提下，本解析器**接受两种写法**：
/// `URGENT` 是 `ERROR` 的业务别名（前端下拉框照文档画「紧急」），落库统一写 `ERROR`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    /// 普通。
    Info,
    /// 提醒。
    Warn,
    /// 紧急（落库 `ERROR`）。
    Urgent,
}

impl NotifyLevel {
    /// 落 `sys_notice.level` 的字符串（`URGENT` → `ERROR`）。
    pub fn as_db_str(self) -> &'static str {
        match self {
            NotifyLevel::Info => "INFO",
            NotifyLevel::Warn => "WARN",
            NotifyLevel::Urgent => "ERROR",
        }
    }

    /// 配置里的字符串（`URGENT` 的原样，回显给前端）。
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyLevel::Info => "INFO",
            NotifyLevel::Warn => "WARN",
            NotifyLevel::Urgent => "URGENT",
        }
    }

    /// 解析（大小写不敏感；`ERROR` 也认，历史数据/手写配置不会因为别名报错）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "INFO" => Some(NotifyLevel::Info),
            "WARN" => Some(NotifyLevel::Warn),
            "URGENT" | "ERROR" => Some(NotifyLevel::Urgent),
            _ => None,
        }
    }
}

/// `targets` 解析出 0 个收件人时的处理（`config.notify.skip_if_empty`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipIfEmpty {
    /// 静默通过（只记 trace）。
    Pass,
    /// 通过 + WARN 日志（默认）。
    Warn,
    /// 整笔回滚（配置错误才这么配）。
    Error,
}

impl SkipIfEmpty {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            SkipIfEmpty::Pass => "PASS",
            SkipIfEmpty::Warn => "WARN",
            SkipIfEmpty::Error => "ERROR",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "PASS" => Some(SkipIfEmpty::Pass),
            "WARN" => Some(SkipIfEmpty::Warn),
            "ERROR" => Some(SkipIfEmpty::Error),
            _ => None,
        }
    }
}

/// 收件人来源（`targets[].type`，与设计文档 §2.6 的 10 值枚举逐字一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyTargetType {
    /// 指定用户。
    User,
    /// 指定角色（按 `role_code`，跨环境稳定）。
    Role,
    /// 部门负责人。
    DeptLeader,
    /// 发起人本人。
    Initiator,
    /// 发起人主管。
    InitiatorSuperior,
    /// 关联任务的负责人。
    TaskOwner,
    /// 关联任务的创建人。
    TaskCreator,
    /// 关联任务的分派人。
    TaskAssigner,
    /// 表单变量里的用户 id。
    FormVar,
    /// 平台超管。
    Admin,
}

impl NotifyTargetType {
    /// 配置 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyTargetType::User => "USER",
            NotifyTargetType::Role => "ROLE",
            NotifyTargetType::DeptLeader => "DEPT_LEADER",
            NotifyTargetType::Initiator => "INITIATOR",
            NotifyTargetType::InitiatorSuperior => "INITIATOR_SUPERIOR",
            NotifyTargetType::TaskOwner => "TASK_OWNER",
            NotifyTargetType::TaskCreator => "TASK_CREATOR",
            NotifyTargetType::TaskAssigner => "TASK_ASSIGNER",
            NotifyTargetType::FormVar => "FORM_VAR",
            NotifyTargetType::Admin => "ADMIN",
        }
    }

    /// 全部取值（错误文案「应为 …」用）。
    pub const ALL: [NotifyTargetType; 10] = [
        NotifyTargetType::User,
        NotifyTargetType::Role,
        NotifyTargetType::DeptLeader,
        NotifyTargetType::Initiator,
        NotifyTargetType::InitiatorSuperior,
        NotifyTargetType::TaskOwner,
        NotifyTargetType::TaskCreator,
        NotifyTargetType::TaskAssigner,
        NotifyTargetType::FormVar,
        NotifyTargetType::Admin,
    ];

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        let upper = raw.trim().to_ascii_uppercase();
        NotifyTargetType::ALL
            .into_iter()
            .find(|kind| kind.as_str() == upper)
    }

    /// 对应的 `assignee_type`（交给 `assignee::resolve` 复用既有解析能力）。
    ///
    /// `DEPT_LEADER` / `TASK_*` 在 `AssigneeKind` 里是 `DeptLeader` / `TaskOwner` …，
    /// 与这里的字符串**逐字相同**，所以可以直接透传 —— 这正是「不复写第二套指派规则」的关键。
    pub fn assignee_type(self) -> &'static str {
        self.as_str()
    }

    /// 该来源必填的键（`None` = 不需要额外键）。
    pub fn required_array_key(self) -> Option<&'static str> {
        match self {
            NotifyTargetType::User => Some("user_ids"),
            NotifyTargetType::Role => Some("role_codes"),
            NotifyTargetType::DeptLeader => Some("dept_ids"),
            _ => None,
        }
    }
}

/// `targets[]` 的一项（强类型；引擎只读它，不回读 JSON）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyTarget {
    /// 来源类型。
    pub target_type: NotifyTargetType,
    /// `USER`：用户 id（已校验为合法 UUID 字符串）。
    pub user_ids: Vec<String>,
    /// `ROLE`：角色编码。
    pub role_codes: Vec<String>,
    /// `DEPT_LEADER`：部门 id。
    pub dept_ids: Vec<String>,
    /// `INITIATOR_SUPERIOR`：上溯层数（默认 1）。
    pub levels: i16,
    /// `FORM_VAR`：表单变量名。
    pub form_var_key: Option<String>,
}

impl NotifyTarget {
    /// 只带类型的最小构造（其余键给默认）。
    fn new(target_type: NotifyTargetType) -> Self {
        Self {
            target_type,
            user_ids: Vec::new(),
            role_codes: Vec::new(),
            dept_ids: Vec::new(),
            levels: 1,
            form_var_key: None,
        }
    }
}

/// 模板二选一（`template_code` | `inline`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyTemplate {
    /// 引用 `sys_notice_template`（管理端可改文案，不用发版）。
    Code {
        /// 模板编码。
        template_code: String,
    },
    /// 内联（走 `render_paths`，支持点号变量）。
    Inline {
        /// 标题模板。
        title: String,
        /// 正文模板。
        content: String,
        /// 正文格式（`TEXT` / `HTML`）。
        content_format: String,
    },
}

impl NotifyTemplate {
    /// 模板里所有需要渲染的字符串（R19 静态扫描占位符用）。
    pub fn templates(&self) -> Vec<&str> {
        match self {
            NotifyTemplate::Code { .. } => Vec::new(),
            NotifyTemplate::Inline { title, content, .. } => vec![title.as_str(), content.as_str()],
        }
    }

    /// 引用模式的模板编码（内联 → `None`）。
    pub fn template_code(&self) -> Option<&str> {
        match self {
            NotifyTemplate::Code { template_code } => Some(template_code),
            NotifyTemplate::Inline { .. } => None,
        }
    }
}

/// `config.notify` 的强类型形态。
#[derive(Debug, Clone, PartialEq)]
pub struct NotifyNodeConfig {
    /// 时机。
    pub when: NotifyWhen,
    /// 收件人（1–5 项）。
    pub targets: Vec<NotifyTarget>,
    /// 是否追加「上一审批节点的最后签字人」。
    pub prev_approver: bool,
    /// 模板。
    pub template: NotifyTemplate,
    /// 通道位掩码（`INBOX=1 / EMAIL=2 / WEBHOOK=4 / SMS=8`；默认 1）。
    pub channels: u16,
    /// 级别。
    pub level: NotifyLevel,
    /// 是否强制送达（跳过 `notify.inbox` 偏好；默认 `false` —— 流程通知多为「知情」）。
    pub force: bool,
    /// 0 收件人时的策略。
    pub skip_if_empty: SkipIfEmpty,
}

/// `400 + 65034`：`config.notify` 形状 / 未知键 / 交叉约束不合法。
pub fn notify_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        400,
        error_code::FLOW_NODE_CONFIG_MISMATCH,
        format!("通知配置非法：{}", message.into()),
    )
}

/// `400 + 65036`：通知模板（编码不存在 / 占位符非法 / 渲染失败）。
pub fn notify_template_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        400,
        error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
        format!("通知模板非法：{}", message.into()),
    )
}

/// 节点配置里的 `config.notify` 对象（`None` = 没配）。
pub fn notify_config_of(config: &Value) -> Option<&Value> {
    config.get("notify")
}

/// 解析 `config.notify`（**形状 + 交叉约束**，不含模板存在性与占位符白名单）。
///
/// # 异常情况
///
/// - `400 + 65034`：不是对象 / 未知键 / `when` 非法 / `targets` 为空或超 5 项 /
///   某来源缺必备键或键用错来源 / `template_code` 与 `inline` 同时给或都没给 / 通道位非法；
/// - `400 + 65036`：`template_code` 格式非法、内联标题正文越界。
pub fn parse_notify_config(value: &Value) -> std::result::Result<NotifyNodeConfig, ApiError> {
    let Some(object) = value.as_object() else {
        return Err(notify_invalid("config.notify 必须是对象"));
    };
    deny_unknown_keys(object, &NOTIFY_CONFIG_KEYS, "config.notify")
        .map_err(|error| notify_invalid(error.msg))?;

    // ---------- when ----------
    let when_raw = required_string(object, "when")
        .map_err(|_| notify_invalid("when 必填（ON_ENTER / ON_LEAVE / ON_TIMEOUT）"))?;
    let when = NotifyWhen::parse(&when_raw).ok_or_else(|| {
        notify_invalid(format!(
            "when 取值非法：{when_raw}（应为 ON_ENTER / ON_LEAVE / ON_TIMEOUT）"
        ))
    })?;

    // ---------- targets ----------
    let targets_value = object
        .get("targets")
        .ok_or_else(|| notify_invalid("targets 必填（1–5 个收件人来源）"))?;
    let targets = parse_notify_targets(targets_value)?;

    // ---------- 模板二选一 ----------
    let template_code =
        optional_string(object, "template_code").map_err(|error| notify_invalid(error.msg))?;
    let inline_present = object.get("inline").is_some_and(|value| !value.is_null());
    let template = match (template_code, inline_present) {
        (Some(code), false) => NotifyTemplate::Code {
            template_code: check_template_code(&code)?,
        },
        (None, true) => parse_inline(&object["inline"])?,
        (Some(_), true) => {
            return Err(notify_invalid(
                "template_code 与 inline 只能二选一（同时给会让「用哪个模板」不可判定）",
            ));
        }
        (None, false) => {
            return Err(notify_invalid(
                "必须配置模板：template_code（引用已有模板）或 inline（自己写标题正文）二选一",
            ));
        }
    };

    // ---------- 其余开关 ----------
    let prev_approver = optional_bool(object, "prev_approver")
        .map_err(|error| notify_invalid(error.msg))?
        .unwrap_or(false);
    let level = match optional_string(object, "level").map_err(|error| notify_invalid(error.msg))? {
        Some(raw) => NotifyLevel::parse(&raw).ok_or_else(|| {
            notify_invalid(format!(
                "level 取值非法：{raw}（应为 INFO / WARN / URGENT）"
            ))
        })?,
        None => NotifyLevel::Info,
    };
    let force = optional_bool(object, "force")
        .map_err(|error| notify_invalid(error.msg))?
        .unwrap_or(false);
    let skip_if_empty = match optional_string(object, "skip_if_empty")
        .map_err(|error| notify_invalid(error.msg))?
    {
        Some(raw) => SkipIfEmpty::parse(&raw).ok_or_else(|| {
            notify_invalid(format!(
                "skip_if_empty 取值非法：{raw}（应为 PASS / WARN / ERROR）"
            ))
        })?,
        None => SkipIfEmpty::Warn,
    };
    let channels = parse_channels(object)?;

    // ---------- 交叉约束：ON_TIMEOUT 必须有停留时长 ----------
    // 引擎侧「等 timeout_hours」是这个时机唯一的语义来源；没有它，节点永远等不到通知。
    // 这条不做，用户会配出一个「永远不会发」的节点 —— 正是 R19 要拦的静默失效。
    if when.is_deferred() {
        // `timeout_hours` 不在 config.notify 里，而在节点行上（`biz_flow_node.timeout_hours`），
        // 由 `graph::FlowGraph::validate_with` 的 R19 结合节点行一起判（见 graph.rs R19 ②）。
    }

    Ok(NotifyNodeConfig {
        when,
        targets,
        prev_approver,
        template,
        channels,
        level,
        force,
        skip_if_empty,
    })
}

/// `targets` 数组：1–5 项，逐项形状校验。
fn parse_notify_targets(value: &Value) -> std::result::Result<Vec<NotifyTarget>, ApiError> {
    let Some(items) = value.as_array() else {
        return Err(notify_invalid("targets 必须是数组"));
    };
    if items.is_empty() {
        return Err(notify_invalid(
            "targets 至少要有一项（没人可发的通知节点没有意义）",
        ));
    }
    if items.len() > MAX_NOTIFY_TARGETS {
        return Err(notify_invalid(format!(
            "targets 有 {} 项，超过上限 {MAX_NOTIFY_TARGETS}（要发给更多人请用角色或部门，不要堆 20 个「指定用户」）",
            items.len()
        )));
    }
    let mut out: Vec<NotifyTarget> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        out.push(parse_one_target(item, index)?);
    }
    Ok(out)
}

/// 单个 target。
fn parse_one_target(value: &Value, index: usize) -> std::result::Result<NotifyTarget, ApiError> {
    let label = format!("targets[{index}]");
    let Some(object) = value.as_object() else {
        return Err(notify_invalid(format!("{label} 必须是对象")));
    };
    deny_unknown_keys(
        object,
        &NOTIFY_TARGET_KEYS,
        &format!("config.notify.{label}"),
    )
    .map_err(|error| notify_invalid(error.msg))?;

    let type_raw = required_string(object, "type")
        .map_err(|_| notify_invalid(format!("{label}.type 必填")))?;
    let target_type = NotifyTargetType::parse(&type_raw).ok_or_else(|| {
        let allowed: Vec<&str> = NotifyTargetType::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        notify_invalid(format!(
            "{label}.type 取值非法：{type_raw}（应为 {}）",
            allowed.join(" / ")
        ))
    })?;

    let mut target = NotifyTarget::new(target_type);
    target.user_ids = parse_uuid_list(object, "user_ids", &label)?;
    target.dept_ids = parse_uuid_list(object, "dept_ids", &label)?;
    target.role_codes = parse_string_list(object, "role_codes", &label)?;
    if let Some(levels) =
        optional_i64(object, "levels").map_err(|error| notify_invalid(error.msg))?
    {
        if !(1..=10).contains(&levels) {
            return Err(notify_invalid(format!(
                "{label}.levels 必须在 1..10 之间（当前 {levels}）"
            )));
        }
        target.levels = levels as i16;
    }
    target.form_var_key =
        optional_string(object, "form_var_key").map_err(|error| notify_invalid(error.msg))?;

    // 来源 × 必备键：**用错来源的键要报错，不是静默忽略**
    // （静默忽略 = 用户以为配了「发给张三」，实际解析成「发起人」）。
    let required = target_type.required_array_key();
    let filled: Vec<(&str, bool)> = vec![
        ("user_ids", !target.user_ids.is_empty()),
        ("role_codes", !target.role_codes.is_empty()),
        ("dept_ids", !target.dept_ids.is_empty()),
    ];
    for (key, present) in &filled {
        if Some(*key) == required {
            if !present {
                return Err(notify_invalid(format!(
                    "{label}（{}）必须填 {key}",
                    target_type.as_str()
                )));
            }
        } else if *present {
            return Err(notify_invalid(format!(
                "{label}（{}）不允许填 {key}（那是别的来源的键）",
                target_type.as_str()
            )));
        }
    }
    if target_type == NotifyTargetType::FormVar {
        let Some(key) = target.form_var_key.as_deref() else {
            return Err(notify_invalid(format!(
                "{label}（FORM_VAR）必须填 form_var_key（表单里存用户 id 的字段名）"
            )));
        };
        if key.trim().is_empty() || key.chars().count() > 64 {
            return Err(notify_invalid(format!(
                "{label}.form_var_key 不能为空且不超过 64 个字符"
            )));
        }
    } else if target.form_var_key.is_some() {
        return Err(notify_invalid(format!(
            "{label}（{}）不允许填 form_var_key",
            target_type.as_str()
        )));
    }
    if target_type != NotifyTargetType::InitiatorSuperior && object.contains_key("levels") {
        return Err(notify_invalid(format!(
            "{label}（{}）不允许填 levels（只有 INITIATOR_SUPERIOR 需要）",
            target_type.as_str()
        )));
    }
    Ok(target)
}

/// `inline` 对象。
fn parse_inline(value: &Value) -> std::result::Result<NotifyTemplate, ApiError> {
    let Some(object) = value.as_object() else {
        return Err(notify_invalid("inline 必须是对象"));
    };
    deny_unknown_keys(object, &INLINE_KEYS, "config.notify.inline")
        .map_err(|error| notify_invalid(error.msg))?;
    let title = required_string(object, "title")
        .map_err(|_| notify_template_invalid("inline.title 必填"))?;
    let content = required_string(object, "content")
        .map_err(|_| notify_template_invalid("inline.content 必填"))?;
    let title_chars = title.chars().count();
    if title_chars == 0 {
        return Err(notify_template_invalid("inline.title 不能为空"));
    }
    if title_chars > MAX_NOTIFY_TITLE_CHARS {
        return Err(notify_template_invalid(format!(
            "inline.title 有 {title_chars} 个字符，超过上限 {MAX_NOTIFY_TITLE_CHARS}（渲染后要落 sys_notice.title varchar(255)）"
        )));
    }
    let content_chars = content.chars().count();
    if content_chars == 0 {
        return Err(notify_template_invalid("inline.content 不能为空"));
    }
    if content_chars > MAX_NOTIFY_CONTENT_CHARS {
        return Err(notify_template_invalid(format!(
            "inline.content 有 {content_chars} 个字符，超过上限 {MAX_NOTIFY_CONTENT_CHARS}"
        )));
    }
    let content_format = match optional_string(object, "content_format")
        .map_err(|error| notify_invalid(error.msg))?
    {
        Some(raw) => {
            let upper = raw.trim().to_ascii_uppercase();
            if upper != "TEXT" && upper != "HTML" {
                return Err(notify_invalid(format!(
                    "inline.content_format 取值非法：{raw}（应为 TEXT / HTML）"
                )));
            }
            upper
        }
        None => "TEXT".to_string(),
    };
    Ok(NotifyTemplate::Inline {
        title,
        content,
        content_format,
    })
}

/// `template_code` 的格式（`^[A-Z][A-Z0-9_]{0,63}$`，与设计 §2.6 的 pattern 一致）。
fn check_template_code(raw: &str) -> std::result::Result<String, ApiError> {
    let code = raw.trim().to_string();
    if code.chars().count() > MAX_NOTIFY_TEMPLATE_CODE_CHARS {
        return Err(notify_template_invalid(format!(
            "template_code 超过 {MAX_NOTIFY_TEMPLATE_CODE_CHARS} 个字符"
        )));
    }
    let mut chars = code.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => {
            return Err(notify_template_invalid(format!(
                "template_code 格式非法：{code}（应大写字母开头，如 FLOW_NODE_NOTICE）"
            )));
        }
    }
    if !code
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(notify_template_invalid(format!(
            "template_code 格式非法：{code}（只允许大写字母 / 数字 / 下划线）"
        )));
    }
    Ok(code)
}

/// `channels` → 位掩码（`INBOX=1 / EMAIL=2 / WEBHOOK=4 / SMS=8`）。
fn parse_channels(object: &Map<String, Value>) -> std::result::Result<u16, ApiError> {
    let Some(value) = object.get("channels") else {
        return Ok(1);
    };
    if value.is_null() {
        return Ok(1);
    }
    let Some(items) = value.as_array() else {
        return Err(notify_invalid("channels 必须是数组"));
    };
    if items.is_empty() {
        return Err(notify_invalid(
            "channels 不能为空数组（不指定就留空整个键）",
        ));
    }
    let mut mask: u16 = 0;
    for item in items {
        let Some(raw) = item.as_str() else {
            return Err(notify_invalid("channels 的元素必须是字符串"));
        };
        let bit = match raw.trim().to_ascii_uppercase().as_str() {
            "INBOX" => 1u16,
            "EMAIL" => 2,
            "WEBHOOK" => 4,
            "SMS" => 8,
            other => {
                return Err(notify_invalid(format!(
                    "channels 取值非法：{other}（应为 INBOX / EMAIL / WEBHOOK / SMS）"
                )));
            }
        };
        mask |= bit;
    }
    Ok(mask)
}

/// `Vec<uuid>` 形态的数组（每项必须是合法 UUID，错一项就报错点位置）。
fn parse_uuid_list(
    object: &Map<String, Value>,
    key: &str,
    label: &str,
) -> std::result::Result<Vec<String>, ApiError> {
    let items = parse_string_list(object, key, label)?;
    for (index, item) in items.iter().enumerate() {
        if Uuid::parse_str(item.trim()).is_err() {
            return Err(notify_invalid(format!(
                "{label}.{key}[{index}] 不是合法 UUID：{item}"
            )));
        }
    }
    Ok(items
        .into_iter()
        .map(|item| item.trim().to_string())
        .collect())
}

/// `Vec<String>` 形态的数组（去重、去空）。
fn parse_string_list(
    object: &Map<String, Value>,
    key: &str,
    label: &str,
) -> std::result::Result<Vec<String>, ApiError> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err(notify_invalid(format!("{label}.{key} 必须是数组")));
    };
    let max = if key == "role_codes" {
        MAX_NOTIFY_ROLE_CODES
    } else {
        MAX_NOTIFY_TARGET_IDS
    };
    if items.len() > max {
        return Err(notify_invalid(format!(
            "{label}.{key} 有 {} 项，超过上限 {max}",
            items.len()
        )));
    }
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            return Err(notify_invalid(format!(
                "{label}.{key}[{index}] 必须是字符串"
            )));
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(notify_invalid(format!(
                "{label}.{key}[{index}] 不能为空字符串"
            )));
        }
        if !out.contains(&text) {
            out.push(text);
        }
    }
    Ok(out)
}

// ==================== `config.sub`（P3-08，转发到 subprocess 模块） ====================

/// 解析 `config.sub`（子流程节点配置）。
///
/// **实现只有一份**：在 [`super::subprocess::parse_config`]。这里留一个转发名字，是因为
/// `graph::validate_with` 的 R27 与 `parse_task_config` / `parse_notify_config` 排在一起读最顺
/// （R18 用 `node_config::parse_task_config`、R19 用 `parse_notify_config`、R27 用本函数），
/// 而 `SUBPROCESS` 的解析器要跟「发起子实例」的代码住在一起（同一个模块讲清一件事）。
pub fn parse_sub_config(
    value: &Value,
) -> std::result::Result<super::subprocess::SubConfig, super::subprocess::SubConfigError> {
    super::subprocess::parse_config(value)
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

// ==================== P-FLOW-2：`config.notify` 解析器的纯逻辑用例 ====================

#[cfg(test)]
mod notify_config_tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Value {
        json!({
            "when": "ON_ENTER",
            "targets": [
                {"type": "INITIATOR"},
                {"type": "ROLE", "role_codes": ["FINANCE_MANAGER"]}
            ],
            "inline": {"title": "{{instance.title}}", "content": "单号 {{instance.biz_no}}"}
        })
    }

    /// 最小合法配置：默认值全部按文档落地。
    #[test]
    fn minimal_config_uses_documented_defaults() {
        let cfg = parse_notify_config(&valid()).expect("必须解析成功");
        assert_eq!(cfg.when, NotifyWhen::OnEnter);
        assert_eq!(cfg.targets.len(), 2);
        assert_eq!(cfg.targets[0].target_type, NotifyTargetType::Initiator);
        assert_eq!(
            cfg.targets[1].role_codes,
            vec!["FINANCE_MANAGER".to_string()]
        );
        assert!(!cfg.prev_approver, "prev_approver 默认 false");
        assert_eq!(cfg.channels, 1, "通道默认只有站内信");
        assert_eq!(cfg.level, NotifyLevel::Info);
        assert!(!cfg.force, "force 默认 false（流程通知多为知情）");
        assert_eq!(cfg.skip_if_empty, SkipIfEmpty::Warn);
        match cfg.template {
            NotifyTemplate::Inline {
                ref title,
                ref content,
                ref content_format,
            } => {
                assert_eq!(title, "{{instance.title}}");
                assert_eq!(content, "单号 {{instance.biz_no}}");
                assert_eq!(content_format, "TEXT");
            }
            _ => panic!("应该是内联模板"),
        }
    }

    /// 未知键必须报错（`additionalProperties: false`）：拼错的键名比「没配」更难发现。
    #[test]
    fn unknown_key_rejected() {
        let mut value = valid();
        value["templat_code"] = json!("TYPO");
        let error = parse_notify_config(&value).expect_err("未知键必须拒绝");
        assert_eq!(error.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(error.msg.contains("templat_code"), "{}", error.msg);
    }

    /// 模板二选一：两个都给 / 都不给都要报错（库侧 CHECK 兜底，这里给中文）。
    #[test]
    fn template_must_be_exactly_one() {
        let mut both = valid();
        both["template_code"] = json!("FLOW_TODO");
        let error = parse_notify_config(&both).expect_err("两个都给必须拒绝");
        assert!(error.msg.contains("二选一"), "{}", error.msg);

        let neither = json!({"when": "ON_ENTER", "targets": [{"type": "INITIATOR"}]});
        let error = parse_notify_config(&neither).expect_err("都不给必须拒绝");
        assert!(error.msg.contains("必须配置模板"), "{}", error.msg);

        // 引用模式的编码必须能过库侧 CHECK 的格式
        let bad_code = json!({
            "when": "ON_ENTER",
            "targets": [{"type": "INITIATOR"}],
            "template_code": "flow_todo"
        });
        let error = parse_notify_config(&bad_code).expect_err("小写编码必须拒绝");
        assert_eq!(error.code, error_code::FLOW_NOTIFY_TEMPLATE_INVALID);
        assert!(error.msg.contains("大写"), "{}", error.msg);
    }

    /// 来源 × 必备键：用错来源的键必须报错，不许静默忽略。
    #[test]
    fn target_keys_must_match_source() {
        // USER 缺 user_ids
        let missing = json!({"when": "ON_ENTER", "targets": [{"type": "USER"}], "template_code": "FLOW_TODO"});
        let error = parse_notify_config(&missing).expect_err("USER 缺 user_ids 必须拒绝");
        assert!(error.msg.contains("user_ids"), "{}", error.msg);

        // INITIATOR 带了 user_ids（别的来源的键）
        let wrong = json!({
            "when": "ON_ENTER",
            "targets": [{"type": "INITIATOR", "user_ids": ["11111111-1111-1111-1111-111111111111"]}],
            "template_code": "FLOW_TODO"
        });
        let error = parse_notify_config(&wrong).expect_err("用错键必须拒绝");
        assert!(error.msg.contains("不允许填"), "{}", error.msg);

        // USER 的 id 不是 UUID
        let bad_uuid = json!({
            "when": "ON_ENTER",
            "targets": [{"type": "USER", "user_ids": ["not-a-uuid"]}],
            "template_code": "FLOW_TODO"
        });
        let error = parse_notify_config(&bad_uuid).expect_err("非法 UUID 必须拒绝");
        assert!(error.msg.contains("不是合法 UUID"), "{}", error.msg);

        // FORM_VAR 必须给 form_var_key
        let form_var = json!({"when": "ON_ENTER", "targets": [{"type": "FORM_VAR"}], "template_code": "FLOW_TODO"});
        let error = parse_notify_config(&form_var).expect_err("FORM_VAR 缺 form_var_key 必须拒绝");
        assert!(error.msg.contains("form_var_key"), "{}", error.msg);
    }

    /// `when` 三值与 `is_deferred`：只有 `ON_TIMEOUT` 需要等待时长。
    #[test]
    fn when_round_trip_and_deferred() {
        for when in NotifyWhen::ALL {
            assert_eq!(NotifyWhen::parse(when.as_str()), Some(when));
        }
        assert!(NotifyWhen::OnTimeout.is_deferred());
        assert!(!NotifyWhen::OnEnter.is_deferred());
        assert_eq!(
            NotifyWhen::OnEnter.idempotency_segment(),
            "E",
            "幂等键用单字母段"
        );

        let bad = json!({"when": "ON_CLICK", "targets": [{"type": "INITIATOR"}], "template_code": "FLOW_TODO"});
        let error = parse_notify_config(&bad).expect_err("非法 when 必须拒绝");
        assert!(error.msg.contains("ON_CLICK"), "{}", error.msg);
    }

    /// 通道位掩码：多选按位或，非法值报错。
    #[test]
    fn channels_map_to_bitmask() {
        let mut value = valid();
        value["channels"] = json!(["INBOX", "EMAIL", "SMS"]);
        assert_eq!(
            parse_notify_config(&value).expect("解析").channels,
            1 | 2 | 8
        );

        value["channels"] = json!(["PIGEON"]);
        let error = parse_notify_config(&value).expect_err("非法通道必须拒绝");
        assert!(error.msg.contains("PIGEON"), "{}", error.msg);
    }
}
