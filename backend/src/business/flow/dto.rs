//! 审批流程定义 · DTO（请求 / 响应契约）· 批次 D · D1。
//!
//! 依据：`docs/批次D-工作流审批-技术架构.md` §3（流程模型）/ §10.4.3（保存整图的请求样例）。
//!
//! # 命名与格式约定（与任务 / 项目 / 公告模块一致）
//!
//! - 主键 / 外键一律 `String`（`Uuid` 没有 serde 实现，前端也不该解析 UUID 语义）；
//! - 时间戳 `chrono::DateTime<Utc>`（serde 默认 RFC3339）；
//! - 枚举大写字符串（`APPROVAL` / `RATIO` / `AND`），与库里的 CHECK 逐字一致；
//!   请求侧**大小写不敏感**（`parse` 走 `to_ascii_uppercase`），响应侧一律大写。
//!
//! # 「字段缺席 ≠ 清空」
//!
//! 可清空字段（分类 / 描述）用 `Option<Option<T>>`：缺席 = 保持原值，显式 `null` = 清空。
//! 这条规则在公告上踩过坑（漏传 = 清空把截止日默默抹掉），项目与流程都从第一天就用双重 Option。
//!
//! # 节点 / 边的 key 是**用户可见标识**
//!
//! `node_key` / `edge_key` 会出现在报错文案（`检测到环：n_legal → n_finance → n_legal`）
//! 与前端画布上，所以格式收紧到 `^[A-Za-z][A-Za-z0-9_]{0,63}$`（与库 CHECK 一致）。
//! 边的 `from` / `to` 用的是 **key 而不是 id**：编辑器只认 key，id 是服务端内部映射。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use alun::prelude::chrono;

use crate::business::flow::graph::{
    ApprovalMode, AssigneeKind, AssigneeRef, EdgeSpec, FlowGraph, GatewayMode, Issue, NodeSpec,
    NodeType,
};

// ==================== 常量 ====================

/// 流程状态：草稿。
pub const DEF_STATUS_DRAFT: &str = "DRAFT";
/// 流程状态：已发布。
pub const DEF_STATUS_PUBLISHED: &str = "PUBLISHED";
/// 流程状态：已停用。
pub const DEF_STATUS_DISABLED: &str = "DISABLED";
/// 全部流程状态。
pub const DEF_STATUSES: [&str; 3] = [DEF_STATUS_DRAFT, DEF_STATUS_PUBLISHED, DEF_STATUS_DISABLED];

/// 版本状态：草稿（可编辑）。
pub const VERSION_DRAFT: &str = "DRAFT";
/// 版本状态：已发布（只读）。
pub const VERSION_PUBLISHED: &str = "PUBLISHED";
/// 版本状态：已归档（历史实例仍可引用）。
pub const VERSION_ARCHIVED: &str = "ARCHIVED";

/// 流程编码最大长度（与 `biz_flow_def.flow_code VARCHAR(64)` 对齐）。
pub const MAX_CODE_CHARS: usize = 64;
/// 流程名称最大长度。
pub const MAX_NAME_CHARS: usize = 100;
/// 业务类型最大长度（`biz_type VARCHAR(32)`）。
pub const MAX_BIZ_TYPE_CHARS: usize = 32;
/// 分类最大长度（`category VARCHAR(32)`）。
pub const MAX_CATEGORY_CHARS: usize = 32;
/// 描述最大长度（TEXT 列，接口层限长）。
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;
/// 变更备注最大长度（`change_note VARCHAR(500)`）。
pub const MAX_CHANGE_NOTE_CHARS: usize = 500;
/// 节点 / 边 key 的最大长度。
pub const MAX_KEY_CHARS: usize = 64;

// ==================== 归一化（纯函数，可单测） ====================

/// 折叠空白：连续空白压成一个空格，去掉首尾。
pub fn collapse_whitespace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim_end().to_string()
}

/// 流程编码归一化：去空白 → 转小写 → 校验 `^[a-z][a-z0-9_]{1,63}$`。
///
/// 为什么强制小写：编码在报错文案与后端日志里出现，大小写混用会让「同一个流程」看起来像两个。
/// 库里的 CHECK 也是小写口径（`ck_biz_flow_def_code`），应用层先归一化再入库。
///
/// # 错误
///
/// 返回中文原因（handler 直接拼进 `400 + 65005`）。
pub fn normalize_flow_code(raw: &str) -> Result<String, String> {
    let collapsed: String = raw
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    if collapsed.chars().count() < 2 {
        return Err("流程编码至少要 2 个字符".to_string());
    }
    if collapsed.chars().count() > MAX_CODE_CHARS {
        return Err(format!("流程编码不能超过 {MAX_CODE_CHARS} 个字符"));
    }
    let mut chars = collapsed.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return Err("流程编码必须以小写字母开头".to_string()),
    }
    if !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
        return Err("流程编码只能包含小写字母、数字和下划线".to_string());
    }
    Ok(collapsed)
}

/// 名称归一化：折叠空白 → 非空 → ≤100 字符。
pub fn normalize_name(raw: &str) -> Result<String, String> {
    let collapsed = collapse_whitespace(raw);
    if collapsed.is_empty() {
        return Err("流程名称不能为空".to_string());
    }
    if collapsed.chars().count() > MAX_NAME_CHARS {
        return Err(format!("流程名称不能超过 {MAX_NAME_CHARS} 个字符"));
    }
    Ok(collapsed)
}

/// 业务类型归一化：折叠空白 → 非空 → 转大写 → ≤32 字符。
///
/// `biz_type` 是「哪类单据能发起」的开关（`TASK_ACCEPT` / `ORDER` …），必须统一大写，
/// 否则 `task_accept` 与 `TASK_ACCEPT` 会是两个互不相干的流程。
pub fn normalize_biz_type(raw: &str) -> Result<String, String> {
    let collapsed = collapse_whitespace(raw).to_uppercase();
    if collapsed.is_empty() {
        return Err("业务类型不能为空".to_string());
    }
    if collapsed.chars().count() > MAX_BIZ_TYPE_CHARS {
        return Err(format!("业务类型不能超过 {MAX_BIZ_TYPE_CHARS} 个字符"));
    }
    if !collapsed
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err("业务类型只能包含大写字母、数字和下划线".to_string());
    }
    Ok(collapsed)
}

/// 分类归一化：折叠空白，空串 → `None`。
pub fn normalize_category(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = collapse_whitespace(raw);
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_CATEGORY_CHARS {
        return Err(format!("分类不能超过 {MAX_CATEGORY_CHARS} 个字符"));
    }
    Ok(Some(trimmed))
}

/// 描述归一化：trim，空串 → `None`，超长报错。
pub fn normalize_description(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!("流程说明不能超过 {MAX_DESCRIPTION_CHARS} 个字符"));
    }
    Ok(Some(trimmed.to_string()))
}

/// 状态归一化（大小写不敏感；非法值 → `None`）。
pub fn normalize_def_status(raw: &str) -> Option<String> {
    let upper = raw.trim().to_ascii_uppercase();
    DEF_STATUSES
        .iter()
        .find(|status| **status == upper)
        .map(|status| (*status).to_string())
}

/// 节点 / 边 key 校验：`^[A-Za-z][A-Za-z0-9_]{0,63}$`（与库 CHECK 一致）。
pub fn check_key(raw: &str, label: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    if trimmed.chars().count() > MAX_KEY_CHARS {
        return Err(format!("{label}不能超过 {MAX_KEY_CHARS} 个字符"));
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return Err(format!("{label}必须以字母开头")),
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!("{label}只能包含字母、数字和下划线"));
    }
    Ok(trimmed.to_string())
}

/// 双重 Option 反序列化：字段缺席 → `None`，显式 `null` → `Some(None)`，有值 → `Some(Some(v))`。
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== 请求 ====================

/// 流程定义分页（`GET /business/flow/def/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DefPageQuery {
    /// 页码（从 1 开始）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 编码 / 名称关键词（ILIKE）。
    pub keyword: Option<String>,
    /// 状态（`DRAFT` / `PUBLISHED` / `DISABLED`）。
    pub status: Option<String>,
    /// 业务类型（精确匹配，大小写不敏感）。
    pub biz_type: Option<String>,
}

impl DefPageQuery {
    /// 归一化关键词（折叠空白 + 去空 → `None`）。
    pub fn normalized_keyword(&self) -> Option<String> {
        let collapsed = collapse_whitespace(self.keyword.as_deref()?);
        (!collapsed.is_empty()).then_some(collapsed)
    }
}

/// 节点审批人（请求）。
#[derive(Debug, Clone, Deserialize)]
pub struct AssigneeReq {
    /// `USER` / `ROLE` / `DEPT_LEADER`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 目标 id。
    pub ref_id: String,
    /// 顺序（缺省 0）。
    #[serde(default)]
    pub sort_order: Option<i32>,
}

/// 图节点（请求）。
///
/// 字段名与架构 §10.4.3 的样例一致；`type` 是 JS 保留字，用 `#[serde(rename)]` 映射。
#[derive(Debug, Clone, Deserialize)]
pub struct GraphNodeReq {
    /// 节点 key。
    pub key: String,
    /// 名称。
    pub name: String,
    /// 类型（七种）。
    #[serde(rename = "type")]
    pub node_type: String,
    /// 画布 x。
    #[serde(default)]
    pub x: Option<i32>,
    /// 画布 y。
    #[serde(default)]
    pub y: Option<i32>,
    /// 排序。
    #[serde(default)]
    pub sort_order: Option<i32>,
    /// 审批人来源。
    #[serde(default)]
    pub assignee_type: Option<String>,
    /// 主管层数（`INITIATOR_SUPERIOR`）。
    #[serde(default)]
    pub superior_levels: Option<i16>,
    /// `FORM_VAR` 变量名。
    #[serde(default)]
    pub form_var_key: Option<String>,
    /// 会签模式。
    #[serde(default)]
    pub approval_mode: Option<String>,
    /// 会签阈值。
    #[serde(default)]
    pub approve_ratio: Option<f64>,
    /// 一票否决（缺省 true，与库默认一致）。
    #[serde(default)]
    pub reject_veto: Option<bool>,
    /// 空审批人策略。
    #[serde(default)]
    pub empty_assignee: Option<String>,
    /// 自审策略。
    #[serde(default)]
    pub self_approval: Option<String>,
    /// 是否允许机器人审批。
    #[serde(default)]
    pub allow_robot: Option<bool>,
    /// 汇聚模式。
    #[serde(default)]
    pub join_mode: Option<String>,
    /// 分叉模式。
    #[serde(default)]
    pub fork_mode: Option<String>,
    /// 超时小时。
    #[serde(default)]
    pub timeout_hours: Option<i32>,
    /// 超时动作。
    #[serde(default)]
    pub timeout_action: Option<String>,
    /// 提前提醒小时。
    #[serde(default)]
    pub remind_before_hours: Option<i32>,
    /// 最多提醒次数。
    #[serde(default)]
    pub max_remind_times: Option<i16>,
    /// 通过后抄送。
    #[serde(default)]
    pub cc_on_approved: Option<bool>,
    /// 扩展配置。
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// 可枚举审批人。
    #[serde(default)]
    pub assignees: Vec<AssigneeReq>,
}

/// 图连线（请求）。
#[derive(Debug, Clone, Deserialize)]
pub struct GraphEdgeReq {
    /// 边 key。
    pub key: String,
    /// 起点 node key。
    pub from: String,
    /// 终点 node key。
    pub to: String,
    /// 条件 DSL。
    #[serde(default)]
    pub condition: Option<serde_json::Value>,
    /// 条件标签。
    #[serde(default)]
    pub label: Option<String>,
    /// 优先级。
    #[serde(default)]
    pub priority: Option<i32>,
    /// 是否兜底分支。
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// 保存整图（`PUT /business/flow/version/{vid}/graph`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GraphSaveReq {
    /// 节点（整图覆盖）。
    #[serde(default)]
    pub nodes: Vec<GraphNodeReq>,
    /// 连线（整图覆盖）。
    #[serde(default)]
    pub edges: Vec<GraphEdgeReq>,
    /// 变更备注（写进 DRAFT 版本行，发布后成为 `change_note`）。
    #[serde(default)]
    pub change_note: Option<String>,
}

/// 新建流程（`POST /business/flow/def`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DefCreateReq {
    /// 流程编码（必填，小写 + 数字 + 下划线）。
    pub flow_code: String,
    /// 流程名称（必填）。
    pub flow_name: String,
    /// 业务类型（必填，如 `TASK_ACCEPT`）。
    pub biz_type: String,
    /// 分类。
    #[serde(default)]
    pub category: Option<String>,
    /// 说明。
    #[serde(default)]
    pub description: Option<String>,
    /// 是否该业务类型的默认流程。
    #[serde(default)]
    pub is_default: Option<bool>,
    /// 是否允许超时自动通过（D10 的流程级总闸）。
    #[serde(default)]
    pub allow_auto_approve: Option<bool>,
}

/// 编辑流程元信息（`PUT /business/flow/def/{id}`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DefUpdateReq {
    /// 名称（缺席 = 不改）。
    pub flow_name: Option<String>,
    /// 业务类型（缺席 = 不改）。
    pub biz_type: Option<String>,
    /// 分类（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub category: Option<Option<String>>,
    /// 说明（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// 是否默认（缺席 = 不改）。
    pub is_default: Option<bool>,
    /// 是否允许超时自动通过（缺席 = 不改）。
    pub allow_auto_approve: Option<bool>,
}

impl DefUpdateReq {
    /// 是否一个可改字段都没传（全缺席 → 400，避免「空 PUT 假成功」）。
    pub fn is_empty(&self) -> bool {
        self.flow_name.is_none()
            && self.biz_type.is_none()
            && self.category.is_none()
            && self.description.is_none()
            && self.is_default.is_none()
            && self.allow_auto_approve.is_none()
    }
}

/// 另存为新草稿（`POST /business/flow/def/{id}/draft`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DraftReq {
    /// 从哪个版本复制（缺省 = 当前发布版本；没有发布过 → 最新版本）。
    #[serde(default)]
    pub source_version_id: Option<String>,
    /// 变更备注。
    #[serde(default)]
    pub change_note: Option<String>,
}

// ==================== 请求 → 图（纯函数，可单测） ====================

impl GraphSaveReq {
    /// 把请求映射成 [`FlowGraph`]，并做**保存期结构校验**（不做发布校验）。
    ///
    /// 保存期只拦「明显是坏数据」的形态（架构 §10.4.3）：
    ///
    /// - 节点 key / 边 key 格式非法或重复；
    /// - 枚举取值非法（类型 / 审批人来源 / 会签模式 / 网关模式 / 超时动作 …）；
    /// - 边的 `from` / `to` 指向不存在的节点；
    /// - 自环边（`A → A`）、重复边（同 `from`+`to` 两条）。
    ///
    /// **不做**发布校验（无环 / 有 START / 有 END / 度数 / 兜底分支 …）：编辑过程中图必然不完整，
    /// 那些问题由 `POST /version/{vid}/validate` 与 `publish` 拦截（架构 §10.4.3「为什么保存不做发布校验」）。
    ///
    /// # 错误
    ///
    /// 返回中文原因（handler 拼进 `400 + 65005`）。
    pub fn into_graph(&self) -> Result<FlowGraph, String> {
        let mut nodes: Vec<NodeSpec> = Vec::with_capacity(self.nodes.len());
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for raw in &self.nodes {
            let key = check_key(&raw.key, "节点 key")?;
            if !seen.insert(key.clone()) {
                return Err(format!("节点 key {key} 重复"));
            }
            let node_type = NodeType::parse(&raw.node_type)
                .ok_or_else(|| format!("节点 {key} 的类型 {} 非法（只允许 START/APPROVAL/COUNTERSIGN/CONDITION/PARALLEL/CC/END）", raw.node_type))?;
            let name = normalize_name(&raw.name)?;
            let assignee_type = match raw.assignee_type.as_deref() {
                None => None,
                Some(text) if text.trim().is_empty() => None,
                Some(text) => Some(AssigneeKind::parse(text).ok_or_else(|| {
                    format!(
                        "节点 {key} 的审批人来源 {text} 非法（只允许 USER/ROLE/INITIATOR/INITIATOR_SUPERIOR/DEPT_LEADER/TASK_OWNER/TASK_CREATOR/TASK_ASSIGNER/FORM_VAR/ADMIN）"
                    )
                })?),
            };
            let approval_mode = match raw.approval_mode.as_deref() {
                None => None,
                Some(text) if text.trim().is_empty() => None,
                Some(text) => Some(ApprovalMode::parse(text).ok_or_else(|| {
                    format!("节点 {key} 的会签方式 {text} 非法（只允许 ALL/RATIO/ANY）")
                })?),
            };
            let join_mode = parse_gateway(raw.join_mode.as_deref(), "汇聚方式", &key)?;
            let fork_mode = parse_gateway(raw.fork_mode.as_deref(), "分叉方式", &key)?;
            // RATIO 必须带 (0,1] 的阈值：库里有 CHECK 兜底（`ck_biz_flow_node_ratio` +
            // `biz_flow_node_approve_ratio_check`），若只靠库拦，用户拿到的是 500「服务器内部错误」，
            // 所以保存期就在这里给中文 400（发布校验 R9 仍会再判一次，两条路径都要挡住不合法组合）。
            if approval_mode == Some(ApprovalMode::Ratio) {
                match raw.approve_ratio {
                    Some(ratio) if ratio > 0.0 && ratio <= 1.0 => {}
                    Some(ratio) => {
                        return Err(format!(
                            "节点 {key} 的通过比例必须在 (0,1] 之间（当前 {ratio}）"
                        ));
                    }
                    None => {
                        return Err(format!(
                            "节点 {key} 选择了「按比例」会签，但没填通过比例（如 0.6）"
                        ));
                    }
                }
            }
            let mut assignees: Vec<AssigneeRef> = Vec::with_capacity(raw.assignees.len());
            for (index, assignee) in raw.assignees.iter().enumerate() {
                let kind = AssigneeKind::parse(&assignee.kind).ok_or_else(|| {
                    format!(
                        "节点 {key} 的审批人类型 {} 非法（只允许 USER/ROLE/DEPT_LEADER 落行）",
                        assignee.kind
                    )
                })?;
                if !kind.is_enumerable() {
                    return Err(format!(
                        "节点 {key} 的审批人类型 {} 不能落行（只有 USER/ROLE/DEPT_LEADER 是可枚举来源）",
                        kind.as_str()
                    ));
                }
                let ref_id = assignee.ref_id.trim().to_string();
                if ref_id.is_empty() {
                    return Err(format!("节点 {key} 有审批人缺少 ref_id"));
                }
                assignees.push(AssigneeRef {
                    kind: kind.as_str().to_string(),
                    ref_id,
                    sort_order: assignee.sort_order.unwrap_or(index as i32),
                });
            }
            // 去重（同 kind + ref_id 只留一条 —— 库里的主键也会拦，但这里先给中文文案）
            let mut deduped: Vec<AssigneeRef> = Vec::with_capacity(assignees.len());
            for assignee in assignees {
                if !deduped
                    .iter()
                    .any(|kept| kept.kind == assignee.kind && kept.ref_id == assignee.ref_id)
                {
                    deduped.push(assignee);
                }
            }

            let empty_assignee = parse_choice(
                raw.empty_assignee.as_deref(),
                &EMPTY_ASSIGNEE_POLICIES,
                "空审批人策略",
                &key,
                "SKIP",
            )?;
            let self_approval = parse_choice(
                raw.self_approval.as_deref(),
                &SELF_APPROVAL_POLICIES,
                "自审策略",
                &key,
                "SKIP",
            )?;
            let timeout_action = parse_choice(
                raw.timeout_action.as_deref(),
                &TIMEOUT_ACTIONS,
                "超时动作",
                &key,
                "REMIND",
            )?;

            nodes.push(NodeSpec {
                key,
                name,
                node_type,
                assignee_type,
                approval_mode,
                approve_ratio: raw.approve_ratio,
                reject_veto: raw.reject_veto.unwrap_or(true),
                join_mode,
                fork_mode,
                form_var_key: raw
                    .form_var_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string),
                superior_levels: raw.superior_levels.unwrap_or(1),
                assignees: deduped,
                empty_assignee,
                self_approval,
                allow_robot: raw.allow_robot.unwrap_or(false),
                timeout_hours: raw.timeout_hours,
                timeout_action,
                remind_before_hours: raw.remind_before_hours,
                max_remind_times: raw.max_remind_times.unwrap_or(3),
                cc_on_approved: raw.cc_on_approved.unwrap_or(false),
                x: raw.x.unwrap_or(0),
                y: raw.y.unwrap_or(0),
                sort_order: raw.sort_order.unwrap_or(0),
                config: raw
                    .config
                    .clone()
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
            });
        }

        let keys: std::collections::HashSet<&str> =
            nodes.iter().map(|node| node.key.as_str()).collect();
        let mut edges: Vec<EdgeSpec> = Vec::with_capacity(self.edges.len());
        let mut edge_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pairs: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for raw in &self.edges {
            let key = check_key(&raw.key, "连线 key")?;
            if !edge_keys.insert(key.clone()) {
                return Err(format!("连线 key {key} 重复"));
            }
            let from = raw.from.trim().to_string();
            let to = raw.to.trim().to_string();
            if !keys.contains(from.as_str()) {
                return Err(format!("连线 {key} 的起点 {from} 不存在"));
            }
            if !keys.contains(to.as_str()) {
                return Err(format!("连线 {key} 的终点 {to} 不存在"));
            }
            if from == to {
                return Err(format!(
                    "连线 {key} 是自环（{from} → {to}），流程不允许自环"
                ));
            }
            if !pairs.insert((from.clone(), to.clone())) {
                return Err(format!(
                    "连线 {from} → {to} 重复（两个节点之间只允许一条连线）"
                ));
            }
            edges.push(EdgeSpec {
                key,
                from,
                to,
                condition: raw.condition.clone(),
                label: raw
                    .label
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(100).collect()),
                priority: raw.priority.unwrap_or(0),
                is_default: raw.is_default.unwrap_or(false),
            });
        }

        Ok(FlowGraph { nodes, edges })
    }

    /// 变更备注归一化（trim + 截断到 500 字符）。
    pub fn normalized_change_note(&self) -> Option<String> {
        self.change_note
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| text.chars().take(MAX_CHANGE_NOTE_CHARS).collect())
    }
}

fn parse_gateway(raw: Option<&str>, label: &str, node_key: &str) -> Result<GatewayMode, String> {
    match raw {
        None => Ok(GatewayMode::And),
        Some(text) if text.trim().is_empty() => Ok(GatewayMode::And),
        Some(text) => GatewayMode::parse(text)
            .ok_or_else(|| format!("节点 {node_key} 的{label} {text} 非法（只允许 AND/OR）")),
    }
}

/// 空审批人策略（架构 §4.3.4 X2）。
pub const EMPTY_ASSIGNEE_POLICIES: [&str; 3] = ["SKIP", "ERROR", "FALLBACK_ADMIN"];
/// 自审策略（架构 §4.3.5 X3）。
pub const SELF_APPROVAL_POLICIES: [&str; 3] = ["ALLOW", "SKIP", "FORBID"];
/// 超时动作（架构 §4.9.1 / D10）。
pub const TIMEOUT_ACTIONS: [&str; 4] = ["REMIND", "AUTO_APPROVE", "AUTO_REJECT", "ESCALATE"];

/// 受限取值字段解析：缺席 / 空白 → 默认值；非法 → 中文错误（点名节点与允许值）。
fn parse_choice(
    raw: Option<&str>,
    allowed: &[&str],
    label: &str,
    node_key: &str,
    fallback: &str,
) -> Result<String, String> {
    let Some(text) = raw else {
        return Ok(fallback.to_string());
    };
    let upper = text.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return Ok(fallback.to_string());
    }
    if allowed.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(format!(
            "节点 {node_key} 的{label} {text} 非法（只允许 {}）",
            allowed.join("/")
        ))
    }
}

// ==================== 响应 ====================

/// 流程定义（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct DefRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类（空串 = 无）。
    pub category: String,
    /// 说明（空串 = 无）。
    pub description: String,
    /// 状态（`DRAFT` / `PUBLISHED` / `DISABLED`）。
    pub status: String,
    /// 当前发布版本 id（未发布 → `null`）。
    pub current_version_id: Option<String>,
    /// 当前发布版本号（未发布 → 0）。
    pub current_version_no: i32,
    /// 是否该业务类型的默认流程。
    pub is_default: bool,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: bool,
    /// 当前版本的节点数（未发布 → 0）。
    pub node_count: i64,
    /// 当前版本的边数（未发布 → 0）。
    pub edge_count: i64,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 流程定义分页出参（前四个字段与框架 `PageData<T>` 逐字一致）。
#[derive(Debug, Clone, Serialize)]
pub struct DefPageRes {
    /// 当前页。
    pub list: Vec<DefRes>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 新建流程回执。
#[derive(Debug, Clone, Serialize)]
pub struct DefSaveRes {
    /// 流程 id。
    pub id: String,
    /// 初始草稿版本 id。
    pub version_id: String,
    /// 版本号（新建 = 1）。
    pub version_no: i32,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
}

/// 节点审批人（响应）。
#[derive(Debug, Clone, Serialize)]
pub struct AssigneeRes {
    /// 类型（`USER` / `ROLE` / `DEPT_LEADER`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 目标 id。
    pub ref_id: String,
    /// 顺序。
    pub sort_order: i32,
}

/// 图节点（响应）。
#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeRes {
    /// 节点 key。
    pub key: String,
    /// 名称。
    pub name: String,
    /// 类型。
    #[serde(rename = "type")]
    pub node_type: String,
    /// 画布 x。
    pub x: i32,
    /// 画布 y。
    pub y: i32,
    /// 排序。
    pub sort_order: i32,
    /// 审批人来源。
    pub assignee_type: Option<String>,
    /// 主管层数。
    pub superior_levels: i16,
    /// `FORM_VAR` 变量名。
    pub form_var_key: Option<String>,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值。
    pub approve_ratio: Option<f64>,
    /// 一票否决。
    pub reject_veto: bool,
    /// 空审批人策略。
    pub empty_assignee: String,
    /// 自审策略。
    pub self_approval: String,
    /// 是否允许机器人。
    pub allow_robot: bool,
    /// 汇聚模式。
    pub join_mode: String,
    /// 分叉模式。
    pub fork_mode: String,
    /// 超时小时。
    pub timeout_hours: Option<i32>,
    /// 超时动作。
    pub timeout_action: String,
    /// 提前提醒小时。
    pub remind_before_hours: Option<i32>,
    /// 最多提醒次数。
    pub max_remind_times: i16,
    /// 通过后抄送。
    pub cc_on_approved: bool,
    /// 扩展配置。
    pub config: serde_json::Value,
    /// 可枚举审批人。
    pub assignees: Vec<AssigneeRes>,
}

/// 图连线（响应）。
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdgeRes {
    /// 边 key。
    pub key: String,
    /// 起点 key。
    pub from: String,
    /// 终点 key。
    pub to: String,
    /// 条件 DSL。
    pub condition: Option<serde_json::Value>,
    /// 条件标签。
    pub label: Option<String>,
    /// 优先级。
    pub priority: i32,
    /// 是否兜底。
    pub is_default: bool,
}

/// 版本摘要（版本列表项）。
#[derive(Debug, Clone, Serialize)]
pub struct VersionSummaryRes {
    /// 版本 id。
    pub id: String,
    /// 版本号。
    pub version_no: i32,
    /// 版本状态（`DRAFT` / `PUBLISHED` / `ARCHIVED`）。
    pub version_status: String,
    /// 是否当前版本。
    pub is_current: bool,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 图 hash（未发布 → `null`）。
    pub graph_hash: Option<String>,
    /// 变更备注。
    pub change_note: Option<String>,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人 id。
    pub published_by: Option<String>,
    /// 发布人姓名（空串 = 未知 / 机器人）。
    pub published_by_name: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 版本列表（`GET /business/flow/def/{id}/versions`）。
#[derive(Debug, Clone, Serialize)]
pub struct VersionListRes {
    /// 版本（按版本号倒序）。
    pub list: Vec<VersionSummaryRes>,
}

/// 版本详情（`GET /business/flow/version/{vid}`）。
#[derive(Debug, Clone, Serialize)]
pub struct VersionRes {
    /// 版本摘要。
    #[serde(flatten)]
    pub summary: VersionSummaryRes,
    /// 所属流程 id。
    pub def_id: String,
    /// 所属流程编码。
    pub flow_code: String,
    /// 所属流程名称。
    pub flow_name: String,
    /// 是否可编辑（只有 `DRAFT` 可编辑 —— 前端据此禁用画布）。
    pub editable: bool,
    /// 快照是否已生成（PUBLISHED 才有）。
    pub has_snapshot: bool,
    /// 节点。
    pub nodes: Vec<GraphNodeRes>,
    /// 连线。
    pub edges: Vec<GraphEdgeRes>,
}

/// 保存整图回执（架构 §10.4.3）。
#[derive(Debug, Clone, Serialize)]
pub struct GraphSaveRes {
    /// 版本 id。
    pub version_id: String,
    /// 版本号。
    pub version_no: i32,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// **不阻断保存、但发布会拦**的提示（架构 §10.4.3：`warnings`）。
    pub warnings: Vec<String>,
}

/// 一条校验不通过项（响应）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueRes {
    /// 规则编号（`R1`–`R15`）。
    pub rule: String,
    /// 业务错误码。
    pub code: i32,
    /// 中文文案。
    pub message: String,
    /// 涉及节点。
    pub nodes: Vec<String>,
}

impl From<&Issue> for IssueRes {
    fn from(issue: &Issue) -> Self {
        Self {
            rule: issue.rule.to_string(),
            code: issue.code,
            message: issue.message.clone(),
            nodes: issue.nodes.clone(),
        }
    }
}

/// 只校验不保存（`POST /business/flow/version/{vid}/validate`）。
#[derive(Debug, Clone, Serialize)]
pub struct ValidateRes {
    /// 版本 id。
    pub version_id: String,
    /// 版本号。
    pub version_no: i32,
    /// 全部通过 → `true`。
    pub passed: bool,
    /// 不通过项（**全部**返回，不是第一个）。
    pub issues: Vec<IssueRes>,
    /// 通过时也会给出的图规模（发布上限提示）。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
}

/// 发布回执。
#[derive(Debug, Clone, Serialize)]
pub struct PublishRes {
    /// 流程 id。
    pub def_id: String,
    /// 发布后的版本 id。
    pub version_id: String,
    /// 版本号。
    pub version_no: i32,
    /// 流程状态（发布后 = `PUBLISHED`）。
    pub status: String,
    /// 图 hash（sha256）。
    pub graph_hash: String,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 发布时间。
    pub published_at: chrono::DateTime<chrono::Utc>,
}

/// 停用回执。
#[derive(Debug, Clone, Serialize)]
pub struct OfflineRes {
    /// 流程 id。
    pub id: String,
    /// 停用后的状态（`DISABLED`）。
    pub status: String,
    /// 当前版本号（停用不动版本）。
    pub current_version_no: i32,
}

/// 删除回执。
#[derive(Debug, Clone, Serialize)]
pub struct DefDeleteRes {
    /// 是否已删除。
    pub deleted: bool,
    /// 编码（软删后可被新流程复用，回执里带上便于对账）。
    pub flow_code: String,
}

/// 另存草稿回执。
#[derive(Debug, Clone, Serialize)]
pub struct DraftRes {
    /// 流程 id。
    pub def_id: String,
    /// 新草稿版本 id。
    pub version_id: String,
    /// 新版本号。
    pub version_no: i32,
    /// 复制来源版本号。
    pub source_version_no: i32,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
}

// ==================== 单元测试 ====================

// ==================== 运行侧：实例 / 待办 / 抄送（D2） ====================
//
// 依据：卡面 t_3715c446「4. 实例侧 API（12–23）」+ 架构 §10.1.2 / §10.1.3 / §10.1.4。
//
// 与定义侧同一套约定：主键一律 `String`、时间 `chrono::DateTime<Utc>`、
// 枚举大写（与库 CHECK 逐字一致）、可空文本降级空串。

/// 实例状态全集（与 `biz_flow_instance.status` 的 CHECK 逐字一致）。
pub const INSTANCE_STATUSES: [&str; 5] =
    ["PENDING", "APPROVED", "REJECTED", "CANCELLED", "WITHDRAWN"];

/// 待办状态全集（与 `biz_flow_task.status` 的 CHECK 逐字一致）。
pub const TASK_STATUSES: [&str; 6] = [
    "PENDING",
    "APPROVED",
    "REJECTED",
    "CANCELLED",
    "SKIPPED",
    "TRANSFERRED",
];

/// 待办种类全集。
pub const TASK_KINDS: [&str; 2] = ["APPROVAL", "RESUBMIT"];

/// 实例标题最大长度（`title VARCHAR(200)`）。
pub const MAX_INSTANCE_TITLE_CHARS: usize = 200;

/// 审批意见最大长度（`comment VARCHAR(1000)`）。
pub const MAX_FLOW_COMMENT_CHARS: usize = 1000;

/// 驳回意见最小字数（卡面「驳回（必填意见 ≥5 字）」）。
pub const MIN_REJECT_COMMENT_CHARS: usize = 5;

/// 单次批量审批上限（架构 §10.1.3 #28）。
pub const MAX_BATCH_TASKS: usize = 50;

/// 单次加签上限（架构 §4.8.2）。
pub const MAX_ADD_SIGN_USERS: usize = 10;

/// 校验实例状态取值（大小写不敏感）。
pub fn normalize_instance_status(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_uppercase();
    INSTANCE_STATUSES
        .into_iter()
        .find(|candidate| *candidate == normalized)
        .map(str::to_string)
}

/// 归一化标题（必填、去多余空白、限长）。
pub fn normalize_instance_title(raw: &str) -> Result<String, String> {
    let collapsed = collapse_whitespace(raw);
    if collapsed.is_empty() {
        return Err("审批单标题不能为空".to_string());
    }
    if collapsed.chars().count() > MAX_INSTANCE_TITLE_CHARS {
        return Err(format!(
            "审批单标题不能超过 {MAX_INSTANCE_TITLE_CHARS} 个字符"
        ));
    }
    Ok(collapsed)
}

/// 归一化审批意见（可选；限长；空 → `None`）。
pub fn normalize_comment(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(text) = raw else { return Ok(None) };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_FLOW_COMMENT_CHARS {
        return Err(format!("审批意见不能超过 {MAX_FLOW_COMMENT_CHARS} 个字符"));
    }
    Ok(Some(trimmed.to_string()))
}

/// 归一化驳回意见（**必填且 ≥ 5 字**，卡面「驳回（必填意见 ≥5 字）」）。
pub fn normalize_reject_comment(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("驳回必须填写意见".to_string());
    }
    let count = trimmed.chars().count();
    if count < MIN_REJECT_COMMENT_CHARS {
        return Err(format!(
            "驳回意见至少 {MIN_REJECT_COMMENT_CHARS} 个字（当前 {count} 个）"
        ));
    }
    if count > MAX_FLOW_COMMENT_CHARS {
        return Err(format!("驳回意见不能超过 {MAX_FLOW_COMMENT_CHARS} 个字符"));
    }
    Ok(trimmed.to_string())
}

/// 发起审批的请求。
#[derive(Debug, Clone, Deserialize)]
pub struct InstanceCreateReq {
    /// 流程定义 id（与 `flow_code` / `biz_type` 三选一）。
    #[serde(default)]
    pub def_id: Option<String>,
    /// 流程编码（三选一）。
    #[serde(default)]
    pub flow_code: Option<String>,
    /// 业务类型（三选一；未给 def/flow_code 时取该类型的默认发布流程）。
    #[serde(default)]
    pub biz_type: Option<String>,
    /// 业务单据 id（可选；给了就会做「同一单据只能有一个在跑实例」的校验）。
    #[serde(default)]
    pub biz_id: Option<String>,
    /// 业务单号（展示用）。
    #[serde(default)]
    pub biz_no: Option<String>,
    /// 标题（必填）。
    pub title: String,
    /// 表单数据（条件表达式的作用域，发起时冻结）。
    #[serde(default)]
    pub form_data: Option<Value>,
}

/// 实例分页查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstancePageQuery {
    /// 页码。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数。
    #[serde(default)]
    pub page_size: Option<u64>,
    /// 单据号 / 标题关键词。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 流程编码。
    #[serde(default)]
    pub flow_code: Option<String>,
    /// 业务类型。
    #[serde(default)]
    pub biz_type: Option<String>,
}

impl InstancePageQuery {
    /// 归一化关键词（空 → `None`）。
    pub fn normalized_keyword(&self) -> Option<String> {
        self.keyword
            .as_deref()
            .map(collapse_whitespace)
            .filter(|value| !value.is_empty())
    }
}

/// 撤回请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WithdrawReq {
    /// 撤回理由（可选）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 同意请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApproveReq {
    /// 审批意见（可选）。
    #[serde(default)]
    pub comment: Option<String>,
}

/// 驳回请求。
#[derive(Debug, Clone, Deserialize)]
pub struct RejectReq {
    /// 驳回意见（必填，≥ 5 字）。
    pub comment: String,
    /// 退回目标（`PREV` 默认 / `INITIATOR` / `NONE` / 节点 key）。
    #[serde(default)]
    pub return_to: Option<String>,
}

/// 转办请求。
#[derive(Debug, Clone, Deserialize)]
pub struct TransferReq {
    /// 目标用户 id。
    pub to_user_id: String,
    /// 转办理由。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 加签请求。
#[derive(Debug, Clone, Deserialize)]
pub struct AddSignReq {
    /// 被加签人 id 列表（≤ 10）。
    #[serde(default)]
    pub user_ids: Vec<String>,
}

/// 批量审批请求。
#[derive(Debug, Clone, Deserialize)]
pub struct BatchReq {
    /// 待办 id 列表（≤ 50）。
    #[serde(default)]
    pub task_ids: Vec<String>,
    /// 动作（`APPROVE` / `REJECT`）。
    pub action: String,
    /// 意见（驳回时必填 ≥ 5 字）。
    #[serde(default)]
    pub comment: Option<String>,
    /// 退回目标（驳回时用，默认 `PREV`）。
    #[serde(default)]
    pub return_to: Option<String>,
}

/// 我的待办 / 已办 / 抄送的分页查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MyTaskQuery {
    /// 页码。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数。
    #[serde(default)]
    pub page_size: Option<u64>,
}

// ---------- 出参 ----------

/// 实例摘要。
#[derive(Debug, Clone, Serialize)]
pub struct InstanceRes {
    /// 主键。
    pub id: String,
    /// 单据号。
    pub instance_no: String,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 绑定版本号（架构 D2：实例绑版本）。
    pub version_no: i32,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id（空串 = 无）。
    pub biz_id: String,
    /// 业务单号（空串 = 无）。
    pub biz_no: String,
    /// 标题。
    pub title: String,
    /// 状态。
    pub status: String,
    /// 发起人 id。
    pub initiator_id: String,
    /// 发起人姓名。
    pub initiator_name: String,
    /// 发起人部门 id（空串 = 无）。
    pub initiator_dept_id: String,
    /// **表单数据**（发起时冻结在 `biz_flow_instance.form_data`）。
    ///
    /// D5 缺口 1：D2 的实例详情不下发它 → 发起时填的内容前端读不回来。现在随摘要一起下发，
    /// 前端读 `detail.instance.form_data` 即可（无表单 → `{}`）。
    pub form_data: Value,
    /// **自动通过节点数**（坑 3 的审计口径，详情页必须显著提示）。
    pub auto_passed_nodes: i32,
    /// 最大退回轮次（打回次数 = `round_total - 1`）。
    pub round_total: i32,
    /// 结束说明。
    pub result_comment: String,
    /// 发起时间。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 我在本单上的待办 id（没有 → 空串）。
    pub my_task_id: String,
}

/// 实例分页出参。
#[derive(Debug, Clone, Serialize)]
pub struct InstancePageRes {
    /// 当前页。
    pub list: Vec<InstanceRes>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 节点进度。
#[derive(Debug, Clone, Serialize)]
pub struct NodeProgressRes {
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 节点类型。
    pub node_type: String,
    /// 轮次（退回一次就 +1，第 1 轮历史保留）。
    pub round: i32,
    /// 状态。
    pub status: String,
    /// 会签模式。
    pub approval_mode: String,
    /// 会签阈值（0 = 非 RATIO）。
    pub approve_ratio: f64,
    /// 应到人数。
    pub approver_count: i32,
    /// 已同意（`COUNT(*)` 现算）。
    pub approved_count: i32,
    /// 已驳回。
    pub rejected_count: i32,
    /// 是否自动通过。
    pub is_auto_passed: bool,
    /// 条件命中的边。
    pub taken_edge_key: String,
    /// 激活时间。
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<i64>,
}

/// 待办 / 时间线条目。
#[derive(Debug, Clone, Serialize)]
pub struct TaskRes {
    /// 待办 id。
    pub id: String,
    /// 实例 id。
    pub instance_id: String,
    /// 单据号（列表场景填）。
    pub instance_no: String,
    /// 单据标题（列表场景填）。
    pub title: String,
    /// 流程名称（D5 缺口 2：待办卡片要显示「哪个流程」）。
    pub flow_name: String,
    /// 发起人姓名（D5 缺口 2；发起人已删 → 空串）。
    pub initiator_name: String,
    /// 当前节点中文名（D5 缺口 2；取 `biz_flow_instance_node.node_name` 快照）。
    pub current_node_name: String,
    /// 到达时间（= `created_at`；**我的待办按它倒序**）。
    pub arrived_at: chrono::DateTime<chrono::Utc>,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 种类（`APPROVAL` / `RESUBMIT`）。
    pub task_kind: String,
    /// 审批人 id。
    pub assignee_id: String,
    /// 审批人姓名。
    pub assignee_name: String,
    /// 来源（8 种之一）。
    pub assignee_source: String,
    /// 状态。
    pub status: String,
    /// 是否加签。
    pub is_added_sign: bool,
    /// 是否自审。
    pub is_self_approval: bool,
    /// 是否系统代点。
    pub is_auto: bool,
    /// 意见。
    pub comment: String,
    /// 转办来源行。
    pub transferred_from: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 完成时间。
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 抄送条目。
#[derive(Debug, Clone, Serialize)]
pub struct CcRes {
    /// 抄送行 id。
    pub id: String,
    /// 实例 id。
    pub instance_id: String,
    /// 单据号。
    pub instance_no: String,
    /// 单据标题。
    pub title: String,
    /// 流程名称。
    pub flow_name: String,
    /// 发起人姓名（已删 → 空串）。
    pub initiator_name: String,
    /// 节点中文名（节点行缺失 → 空串）。
    pub current_node_name: String,
    /// 抄送到达时间（= `created_at`；**抄送列表按它倒序**）。
    pub arrived_at: chrono::DateTime<chrono::Utc>,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 是否已读。
    pub is_read: bool,
    /// 读取时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 实例详情（进度 + 时间线 + 抄送 + 我的可用动作）。
#[derive(Debug, Clone, Serialize)]
pub struct InstanceDetailRes {
    /// 实例摘要。
    pub instance: InstanceRes,
    /// 节点进度（按 round / 到达顺序）。
    pub nodes: Vec<NodeProgressRes>,
    /// 审批时间线（含 SKIPPED / TRANSFERRED 留痕，按 round 分组由前端做）。
    pub timeline: Vec<TaskRes>,
    /// 抄送。
    pub cc: Vec<CcRes>,
    /// 我在本单上的 PENDING 待办（没有 → `null`）。
    pub my_task: Option<TaskRes>,
    /// 自动通过节点数（与 `instance.auto_passed_nodes` 同值，方便前端直接取）。
    pub auto_passed_nodes: i32,
}

/// 审批动作的出参。
#[derive(Debug, Clone, Serialize)]
pub struct ActionRes {
    /// 实例 id。
    pub instance_id: String,
    /// 动作后的实例状态。
    pub instance_status: String,
    /// 当前节点状态（投票类动作）。
    pub node_status: String,
    /// 待办是否已处理完。
    pub done: bool,
    /// 是否幂等命中（重复提交 / 23505 兜底）。
    pub idempotent: bool,
    /// 本次激活的下游节点 key。
    pub next_nodes: Vec<String>,
    /// 自动通过节点数。
    pub auto_passed_nodes: i32,
    /// 应到人数。
    pub approver_count: i32,
    /// 已同意。
    pub approved_count: i32,
    /// 已驳回。
    pub rejected_count: i32,
}

/// 我的待办 / 已办 分页出参。
#[derive(Debug, Clone, Serialize)]
pub struct TaskPageRes {
    /// 当前页。
    pub list: Vec<TaskRes>,
    /// 总数（**跨页总数**，`count(*)` 现算；不再按页内条数回填）。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 抄送分页出参。
#[derive(Debug, Clone, Serialize)]
pub struct CcPageRes {
    /// 当前页。
    pub list: Vec<CcRes>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 批量审批的单条结果。
#[derive(Debug, Clone, Serialize)]
pub struct BatchItemRes {
    /// 待办 id。
    pub task_id: String,
    /// 是否成功。
    pub ok: bool,
    /// 失败码（成功 → 0）。
    pub code: i32,
    /// 失败原因（成功 → 空串）。
    pub message: String,
}

/// 批量审批出参（**逐条独立事务**：一条失败不影响其它条）。
#[derive(Debug, Clone, Serialize)]
pub struct BatchRes {
    /// 提交总数。
    pub total: usize,
    /// 成功数。
    pub succeeded: usize,
    /// 失败数。
    pub failed: usize,
    /// 逐条结果。
    pub items: Vec<BatchItemRes>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flow_code_is_normalized_to_lowercase() {
        assert_eq!(
            normalize_flow_code("  Task_Accept ").unwrap(),
            "task_accept"
        );
        assert!(normalize_flow_code("1abc").is_err(), "不能以数字开头");
        assert!(normalize_flow_code("a").is_err(), "至少 2 个字符");
        assert!(normalize_flow_code("task-accept").is_err(), "不许短横线");
        assert!(normalize_flow_code("任务审批").is_err(), "不许中文");
    }

    #[test]
    fn biz_type_and_names_are_normalized() {
        assert_eq!(normalize_biz_type(" task_accept ").unwrap(), "TASK_ACCEPT");
        assert!(normalize_biz_type("").is_err());
        assert_eq!(normalize_name("  财务   会签  ").unwrap(), "财务 会签");
        assert!(normalize_name("   ").is_err());
        assert_eq!(normalize_category(Some("  ")).unwrap(), None);
        assert_eq!(
            normalize_description(Some("  说明  ")).unwrap(),
            Some("说明".to_string())
        );
    }

    #[test]
    fn keys_must_match_db_check() {
        assert_eq!(check_key(" n_start ", "节点 key").unwrap(), "n_start");
        assert!(check_key("1start", "节点 key").is_err());
        assert!(check_key("n-start", "节点 key").is_err());
        assert!(check_key("", "节点 key").is_err());
    }

    fn node(key: &str, node_type: &str) -> GraphNodeReq {
        GraphNodeReq {
            key: key.to_string(),
            name: format!("节点{key}"),
            node_type: node_type.to_string(),
            x: None,
            y: None,
            sort_order: None,
            assignee_type: Some("INITIATOR".to_string()),
            superior_levels: None,
            form_var_key: None,
            approval_mode: None,
            approve_ratio: None,
            reject_veto: None,
            empty_assignee: None,
            self_approval: None,
            allow_robot: None,
            join_mode: None,
            fork_mode: None,
            timeout_hours: None,
            timeout_action: None,
            remind_before_hours: None,
            max_remind_times: None,
            cc_on_approved: None,
            config: None,
            assignees: Vec::new(),
        }
    }

    fn edge(key: &str, from: &str, to: &str) -> GraphEdgeReq {
        GraphEdgeReq {
            key: key.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            condition: None,
            label: None,
            priority: None,
            is_default: None,
        }
    }

    #[test]
    fn graph_request_maps_to_domain_graph() {
        let req = GraphSaveReq {
            nodes: vec![node("n_start", "START"), node("n_end", "END")],
            edges: vec![edge("e1", "n_start", "n_end")],
            change_note: Some("  初版  ".to_string()),
        };
        let graph = req.into_graph().expect("合法请求必须映射成功");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(req.normalized_change_note().unwrap(), "初版");
    }

    #[test]
    fn graph_request_rejects_bad_shapes() {
        // 重复节点 key
        let req = GraphSaveReq {
            nodes: vec![node("a", "START"), node("a", "END")],
            edges: vec![],
            change_note: None,
        };
        assert!(req.into_graph().unwrap_err().contains("重复"));

        // 悬空边
        let req = GraphSaveReq {
            nodes: vec![node("a", "START"), node("b", "END")],
            edges: vec![edge("e1", "a", "zzz")],
            change_note: None,
        };
        assert!(req.into_graph().unwrap_err().contains("不存在"));

        // 自环
        let req = GraphSaveReq {
            nodes: vec![node("a", "START"), node("b", "END")],
            edges: vec![edge("e1", "a", "a")],
            change_note: None,
        };
        assert!(req.into_graph().unwrap_err().contains("自环"));

        // 重复边
        let req = GraphSaveReq {
            nodes: vec![node("a", "START"), node("b", "END")],
            edges: vec![edge("e1", "a", "b"), edge("e2", "a", "b")],
            change_note: None,
        };
        assert!(req.into_graph().unwrap_err().contains("重复"));

        // 非法类型
        let req = GraphSaveReq {
            nodes: vec![node("a", "GATEWAY")],
            edges: vec![],
            change_note: None,
        };
        assert!(req.into_graph().unwrap_err().contains("非法"));
    }

    #[test]
    fn graph_request_normalizes_enums_case_insensitively() {
        let mut raw = node("n", "countersign");
        raw.approval_mode = Some("ratio".to_string());
        raw.approve_ratio = Some(0.6);
        raw.join_mode = Some("or".to_string());
        raw.assignees = vec![AssigneeReq {
            kind: "role".to_string(),
            ref_id: "00000000-0000-0000-0000-000000000001".to_string(),
            sort_order: None,
        }];
        let req = GraphSaveReq {
            nodes: vec![raw],
            edges: vec![],
            change_note: None,
        };
        let graph = req.into_graph().unwrap();
        let node = &graph.nodes[0];
        assert_eq!(node.node_type, NodeType::Countersign);
        assert_eq!(node.approval_mode, Some(ApprovalMode::Ratio));
        assert_eq!(node.join_mode, GatewayMode::Or);
        assert_eq!(node.assignees.len(), 1);
    }

    #[test]
    fn config_defaults_to_empty_object() {
        let req = GraphSaveReq {
            nodes: vec![node("a", "START")],
            edges: vec![],
            change_note: None,
        };
        let graph = req.into_graph().unwrap();
        assert_eq!(graph.nodes[0].config, json!({}));
        assert!(graph.nodes[0].reject_veto, "缺省一票否决为 true");
    }
}
