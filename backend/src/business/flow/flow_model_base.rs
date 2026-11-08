// ============================================================================
// flow_model_base.rs —— 底座·模型定义：九张表的列清单、JSON/时间/主键参数助手、全部行结构（定义/版本/实例/节点/任务/抄送）、分页与写入入参（NewXxx/DefPatch/PublishWrite/Cancelled*）——被其余部件共同引用、自身不含 SQL 编排
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

//
// SCOPE-EXEMPT: 流程定义 / 版本 / 节点 / 边 / 审批人是**全局配置**（不是谁的业务数据），
// SCOPE-EXEMPT: 可见性由 `business:flow:read` 权限码控制，按部门范围裁剪会导致「看不到自己的流程」。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;

// ==================== 常量：列清单 ====================

/// 流程定义列清单（含当前版本的节点 / 边数）。
pub(super) const DEF_COLUMNS: &str = "\
     d.id, d.flow_code, d.flow_name, d.biz_type, d.category, d.description, d.status, \
     d.current_version_id, d.current_version_no, d.is_default, d.allow_auto_approve, \
     d.created_at, d.updated_at, \
     COALESCE(v.node_count, 0) AS cur_node_count, COALESCE(v.edge_count, 0) AS cur_edge_count";

/// 流程定义 FROM 片段（当前版本 LEFT JOIN —— 未发布也有行）。
pub(super) const DEF_FROM: &str = " FROM biz_flow_def d \
     LEFT JOIN biz_flow_def_version v ON v.id = d.current_version_id";

/// 版本列清单（含发布人姓名）。
pub(super) const VERSION_COLUMNS: &str = "\
     v.id, v.def_id, v.version_no, v.version_status, v.is_current, v.graph_hash, \
     v.node_count, v.edge_count, v.change_note, v.published_at, v.published_by, \
     v.created_at, v.updated_at, \
     COALESCE(NULLIF(u.real_name, ''), u.username, '') AS published_by_name";

/// 版本 FROM 片段。
pub(super) const VERSION_FROM: &str = " FROM biz_flow_def_version v \
     LEFT JOIN sys_user u ON u.id = v.published_by";

/// 节点列清单（`approve_ratio` / `config` 在 SQL 里转成好绑定的形态）。
pub(super) const NODE_COLUMNS: &str = "\
     n.id, n.version_id, n.node_key, n.node_name, n.node_type, n.assignee_type, \
     n.superior_levels, n.form_var_key, n.approval_mode, n.approve_ratio::float8 AS approve_ratio, \
     n.reject_veto, n.empty_assignee, n.self_approval, n.allow_robot, \
     n.fallback_user_id, n.fallback_role_id, n.join_mode, n.fork_mode, \
     n.timeout_hours, n.timeout_action, n.remind_before_hours, n.max_remind_times, \
     n.cc_on_approved, n.position_x, n.position_y, n.sort_order, n.config::text AS config_text";

// ==================== 参数助手 ====================

pub(super) fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

pub(super) fn opt_i32(value: Option<i32>) -> Value {
    match value {
        Some(number) => Value::from(number),
        None => Value::Null,
    }
}

// ==================== 行结构 ====================

/// 流程定义行。
#[derive(Debug, Clone)]
pub struct DefRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类。
    pub category: Option<String>,
    /// 说明。
    pub description: Option<String>,
    /// 状态。
    pub status: String,
    /// 当前发布版本。
    pub current_version_id: Option<Uuid>,
    /// 当前发布版本号。
    pub current_version_no: i32,
    /// 是否默认。
    pub is_default: bool,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: bool,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 当前版本节点数。
    pub cur_node_count: i64,
    /// 当前版本边数。
    pub cur_edge_count: i64,
}

impl DefRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            category: row.get_typed("category")?,
            description: row.get_typed("description")?,
            status: row.get_typed("status")?,
            current_version_id: row.get_typed("current_version_id")?,
            current_version_no: row.get_typed("current_version_no")?,
            is_default: row.get_typed("is_default")?,
            allow_auto_approve: row.get_typed("allow_auto_approve")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            cur_node_count: row.get_typed("cur_node_count")?,
            cur_edge_count: row.get_typed("cur_edge_count")?,
        })
    }
}

/// 版本行。
#[derive(Debug, Clone)]
pub struct VersionRow {
    /// 主键。
    pub id: Uuid,
    /// 所属流程。
    pub def_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 版本状态（`DRAFT` / `PUBLISHED` / `ARCHIVED`）。
    pub version_status: String,
    /// 是否当前版本。
    pub is_current: bool,
    /// 图 hash。
    pub graph_hash: Option<String>,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 变更备注。
    pub change_note: Option<String>,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人。
    pub published_by: Option<Uuid>,
    /// 发布人姓名。
    pub published_by_name: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl VersionRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            def_id: row.get_typed("def_id")?,
            version_no: row.get_typed("version_no")?,
            version_status: row.get_typed("version_status")?,
            is_current: row.get_typed("is_current")?,
            graph_hash: row.get_typed("graph_hash")?,
            node_count: row.get_typed("node_count")?,
            edge_count: row.get_typed("edge_count")?,
            change_note: row.get_typed("change_note")?,
            published_at: row.get_typed("published_at")?,
            published_by: row.get_typed("published_by")?,
            published_by_name: row.get_typed("published_by_name")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否可编辑（只有 DRAFT）。
    pub fn editable(&self) -> bool {
        self.version_status == "DRAFT"
    }
}

// ==================== 读路径 ====================

/// 流程定义分页条件。
#[derive(Debug, Clone, Default)]
pub struct DefFilter {
    /// 编码 / 名称关键词（已归一化）。
    pub keyword: Option<String>,
    /// 状态（已归一化）。
    pub status: Option<String>,
    /// 业务类型（已转大写）。
    pub biz_type: Option<String>,
}

/// 分页结果。
#[derive(Debug, Clone)]
pub struct DefPage {
    /// 当前页。
    pub list: Vec<DefRow>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub size: u64,
}

/// 新建流程定义入参。
#[derive(Debug, Clone)]
pub struct NewDef {
    /// 主键（应用侧生成）。
    pub id: Uuid,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类。
    pub category: Option<String>,
    /// 说明。
    pub description: Option<String>,
    /// 是否默认。
    pub is_default: bool,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: bool,
}

/// 定义可改字段（`None` = 不改）。
#[derive(Debug, Clone, Default)]
pub struct DefPatch {
    /// 名称。
    pub flow_name: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
    /// 分类（`Some(None)` = 清空）。
    pub category: Option<Option<String>>,
    /// 说明（`Some(None)` = 清空）。
    pub description: Option<Option<String>>,
    /// 是否默认。
    pub is_default: Option<bool>,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: Option<bool>,
}

/// 发布写入参数（一次发布的全部字段）。
///
/// 打包成结构体而不是 9 个位置参数：这条路径的参数里有两个 hash / 快照字符串和两个计数，
/// 位置参数一旦顺序搞反（`graph_hash` 与 `graph_snapshot` 互串）编译期发现不了。
#[derive(Debug)]
pub struct PublishWrite<'a> {
    /// 流程 id。
    pub def_id: Uuid,
    /// 要发布的版本 id。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 快照 canonical JSON。
    pub snapshot: &'a str,
    /// 快照 sha256。
    pub graph_hash: &'a str,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 操作人。
    pub operator: Uuid,
}

pub(super) const INSTANCE_COLUMNS: &str = "\
     i.id, i.instance_no, i.def_id, i.version_id, i.version_no, i.flow_code, i.flow_name, \
     i.biz_type, i.biz_id, i.biz_no, i.title, i.status, i.initiator_id, i.initiator_name, \
     i.initiator_dept_id, i.form_data::text AS form_data_text, i.result_comment, \
     i.auto_passed_nodes, i.round_total, i.prev_instance_id, i.revision, \
     i.started_at, i.ended_at, i.due_at, i.created_at, i.updated_at";

pub(super) const NODE_RT_COLUMNS: &str = "\
     n.id, n.instance_id, n.node_id, n.node_key, n.node_name, n.node_type, n.round, n.status, \
     n.approval_mode, n.approve_ratio::float8 AS approve_ratio, n.reject_veto, \
     n.approver_count, n.approved_count, n.rejected_count, n.taken_edge_key, n.is_auto_passed, \
     n.activated_at, n.ended_at, n.duration_ms, n.created_at, n.updated_at";

pub(super) const TASK_COLUMNS: &str = "\
     t.id, t.instance_id, t.instance_node_id, t.node_key, t.round, t.task_kind, t.assignee_id, \
     t.assignee_name, t.assignee_source, t.assignee_source_ref, t.status, t.is_added_sign, \
     t.is_self_approval, t.is_auto, t.comment, t.attachments::text AS attachments_text, \
     t.transferred_from, t.transferred_to, t.due_at, t.remind_count, t.created_at, t.completed_at, \
     t.escalated_from_role, t.escalate_reason, t.assigned_by_rule, \
     (SELECT r.role_name FROM sys_role r \
       WHERE r.id = t.assignee_source_ref AND t.escalate_reason IS NOT NULL) AS escalated_to_role";

pub(super) const CC_COLUMNS: &str = "\
     c.id, c.instance_id, c.node_key, c.round, c.receiver_id, c.receiver_name, c.source_type, \
     c.source_ref, c.is_read, c.read_at, c.created_at";

// ---------- 行结构 ----------

/// 实例行。
#[derive(Debug, Clone)]
pub struct InstanceRow {
    /// 主键。
    pub id: Uuid,
    /// 单据号。
    pub instance_no: String,
    /// 流程定义。
    pub def_id: Uuid,
    /// 绑定版本（架构 D2：引擎只读该版本的快照）。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 流程编码（冗余，防展示期 JOIN 定义表）。
    pub flow_code: String,
    /// 流程名称（冗余）。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 标题。
    pub title: String,
    /// 状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    pub status: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起人部门。
    pub initiator_dept_id: Option<Uuid>,
    /// 表单数据（发起时冻结）。
    pub form_data: Json,
    /// 结束说明。
    pub result_comment: Option<String>,
    /// 自动通过节点数（派生值）。
    pub auto_passed_nodes: i32,
    /// 最大退回轮次。
    pub round_total: i32,
    /// 上一次实例（重新发起时串联）。
    pub prev_instance_id: Option<Uuid>,
    /// CAS 版本号。
    pub revision: i32,
    /// 发起时间。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl InstanceRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let form_text: String = row.get_typed("form_data_text")?;
        Ok(Self {
            id: row.get_typed("id")?,
            instance_no: row.get_typed("instance_no")?,
            def_id: row.get_typed("def_id")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            biz_id: row.get_typed("biz_id")?,
            biz_no: row.get_typed("biz_no")?,
            title: row.get_typed("title")?,
            status: row.get_typed("status")?,
            initiator_id: row.get_typed("initiator_id")?,
            initiator_name: row.get_typed("initiator_name")?,
            initiator_dept_id: row.get_typed("initiator_dept_id")?,
            form_data: serde_json::from_str(&form_text)
                .unwrap_or_else(|_| Json::Object(Default::default())),
            result_comment: row.get_typed("result_comment")?,
            auto_passed_nodes: row.get_typed("auto_passed_nodes")?,
            round_total: row.get_typed("round_total")?,
            prev_instance_id: row.get_typed("prev_instance_id")?,
            revision: row.get_typed("revision")?,
            started_at: row.get_typed("started_at")?,
            ended_at: row.get_typed("ended_at")?,
            due_at: row.get_typed("due_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否进行中。
    pub fn is_pending(&self) -> bool {
        self.status == "PENDING"
    }

    /// 审批人解析上下文（`assignee::InstanceCtx`）。
    pub fn assignee_ctx(&self) -> crate::business::flow::assignee::InstanceCtx {
        crate::business::flow::assignee::InstanceCtx {
            instance_id: self.id,
            initiator_id: self.initiator_id,
            initiator_dept_id: self.initiator_dept_id,
            biz_type: self.biz_type.clone(),
            biz_id: self.biz_id,
            form_data: self.form_data.clone(),
        }
    }
}

/// 实例节点行。
#[derive(Debug, Clone)]
pub struct NodeRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 定义侧节点 id（可空，删版本时 `SET NULL`）。
    pub node_id: Option<Uuid>,
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 节点类型。
    pub node_type: String,
    /// 退回轮次。
    pub round: i32,
    /// 状态。
    pub status: String,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值。
    pub approve_ratio: Option<f64>,
    /// 一票否决。
    pub reject_veto: Option<bool>,
    /// 应到人数。
    pub approver_count: i32,
    /// 已同意（展示冗余）。
    pub approved_count: i32,
    /// 已驳回（展示冗余）。
    pub rejected_count: i32,
    /// 条件节点命中的边。
    pub taken_edge_key: Option<String>,
    /// 是否自动通过。
    pub is_auto_passed: bool,
    /// 激活时间。
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<i64>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl NodeRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            node_id: row.get_typed("node_id")?,
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            node_type: row.get_typed("node_type")?,
            round: row.get_typed("round")?,
            status: row.get_typed("status")?,
            approval_mode: row.get_typed("approval_mode")?,
            approve_ratio: row.get_typed("approve_ratio")?,
            reject_veto: row.get_typed("reject_veto")?,
            approver_count: row.get_typed("approver_count")?,
            approved_count: row.get_typed("approved_count")?,
            rejected_count: row.get_typed("rejected_count")?,
            taken_edge_key: row.get_typed("taken_edge_key")?,
            is_auto_passed: row.get_typed("is_auto_passed")?,
            activated_at: row.get_typed("activated_at")?,
            ended_at: row.get_typed("ended_at")?,
            duration_ms: row.get_typed("duration_ms")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否已经结束（终态）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "APPROVED" | "REJECTED" | "SKIPPED" | "CANCELLED"
        )
    }
}

/// 待办 / 审批记录行。
#[derive(Debug, Clone)]
pub struct TaskRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 实例节点。
    pub instance_node_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 待办种类（`APPROVAL` / `RESUBMIT`）。
    pub task_kind: String,
    /// 审批人。
    pub assignee_id: Uuid,
    /// 审批人姓名快照。
    pub assignee_name: Option<String>,
    /// 来源。
    pub assignee_source: String,
    /// 来源引用（角色 / 部门）。
    pub assignee_source_ref: Option<Uuid>,
    /// 状态。
    pub status: String,
    /// 是否加签。
    pub is_added_sign: bool,
    /// 是否自审。
    pub is_self_approval: bool,
    /// 是否系统代点。
    pub is_auto: bool,
    /// 意见。
    pub comment: Option<String>,
    /// 附件（JSON 数组）。
    pub attachments: Json,
    /// 转办来源。
    pub transferred_from: Option<Uuid>,
    /// 转办去向。
    pub transferred_to: Option<Uuid>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 提醒次数。
    pub remind_count: i16,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 完成时间。
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 升级留痕：原本该审的角色编码（P3-02；非空 = 这条待办是沿升级链派下来的）。
    pub escalated_from_role: Option<String>,
    /// 升级原因：`SKIP_SELF_APPROVAL`（自审剔光）/ `NO_MEMBER`（角色 0 成员）。
    pub escalate_reason: Option<String>,
    /// 派单规则留痕（P3-03）：`ROLE` / `ESCALATED` / `FALLBACK` / `DELEGATED`；`None` = 默认直派。
    pub assigned_by_rule: Option<String>,
    /// 升级**目标角色**的名字（P3-02 展示用：`assignee_source_ref` 指向的那个角色）。
    ///
    /// 由 `TASK_COLUMNS` 里的相关子查询算出（只有升级行才有值）；
    /// 不落业务表 —— 角色改名后时间线应该跟着变，留痕要的是「派给了谁」而不是「当时那个角色叫什么」。
    pub escalated_to_role: Option<String>,
}

impl TaskRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let attachments_text: String = row.get_typed("attachments_text")?;
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            instance_node_id: row.get_typed("instance_node_id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
            task_kind: row.get_typed("task_kind")?,
            assignee_id: row.get_typed("assignee_id")?,
            assignee_name: row.get_typed("assignee_name")?,
            assignee_source: row.get_typed("assignee_source")?,
            assignee_source_ref: row.get_typed("assignee_source_ref")?,
            status: row.get_typed("status")?,
            is_added_sign: row.get_typed("is_added_sign")?,
            is_self_approval: row.get_typed("is_self_approval")?,
            is_auto: row.get_typed("is_auto")?,
            comment: row.get_typed("comment")?,
            attachments: serde_json::from_str(&attachments_text)
                .unwrap_or_else(|_| Json::Array(Vec::new())),
            transferred_from: row.get_typed("transferred_from")?,
            transferred_to: row.get_typed("transferred_to")?,
            due_at: row.get_typed("due_at")?,
            remind_count: row.get_typed("remind_count")?,
            created_at: row.get_typed("created_at")?,
            completed_at: row.get_typed("completed_at")?,
            escalated_from_role: row.get_typed("escalated_from_role")?,
            escalate_reason: row.get_typed("escalate_reason")?,
            assigned_by_rule: row.get_typed("assigned_by_rule")?,
            escalated_to_role: row.get_typed("escalated_to_role")?,
        })
    }

    /// 是否还在待办里。
    pub fn is_pending(&self) -> bool {
        self.status == "PENDING"
    }
}

/// 抄送行。
#[derive(Debug, Clone)]
pub struct CcRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 收件人。
    pub receiver_id: Uuid,
    /// 收件人姓名。
    pub receiver_name: Option<String>,
    /// 来源。
    pub source_type: String,
    /// 来源引用。
    pub source_ref: Option<Uuid>,
    /// 是否已读。
    pub is_read: bool,
    /// 读取时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl CcRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
            receiver_id: row.get_typed("receiver_id")?,
            receiver_name: row.get_typed("receiver_name")?,
            source_type: row.get_typed("source_type")?,
            source_ref: row.get_typed("source_ref")?,
            is_read: row.get_typed("is_read")?,
            read_at: row.get_typed("read_at")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 票数（**唯一权威来源是 `COUNT(*)`**，架构 D15）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// 已同意。
    pub approved: i64,
    /// 已驳回。
    pub rejected: i64,
    /// 未表态。
    pub pending: i64,
    /// 应到（在册票数：`PENDING + APPROVED + REJECTED`，**不含** `SKIPPED` / `TRANSFERRED` / `CANCELLED`）。
    pub total: i64,
}

// ---------- 单据号 ----------

/// 新建实例入参。
#[derive(Debug, Clone)]
pub struct NewInstance {
    /// 主键。
    pub id: Uuid,
    /// 单据号。
    pub instance_no: String,
    /// 流程定义。
    pub def_id: Uuid,
    /// 绑定版本。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 标题。
    pub title: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起人部门。
    pub initiator_dept_id: Option<Uuid>,
    /// 表单数据。
    pub form_data: Json,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 节点「到达」入参。
#[derive(Debug, Clone)]
pub struct NewNode {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 定义侧节点 id。
    pub node_id: Option<Uuid>,
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 节点类型。
    pub node_type: String,
    /// 轮次。
    pub round: i32,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值。
    pub approve_ratio: Option<f64>,
    /// 一票否决。
    pub reject_veto: Option<bool>,
}

/// 新建待办入参。
#[derive(Debug, Clone)]
pub struct NewTask {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 实例节点。
    pub instance_node_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 种类（`APPROVAL` / `RESUBMIT`）。
    pub task_kind: String,
    /// 审批人。
    pub assignee_id: Uuid,
    /// 审批人姓名快照。
    pub assignee_name: Option<String>,
    /// 来源。
    pub assignee_source: String,
    /// 来源引用。
    pub assignee_source_ref: Option<Uuid>,
    /// 是否加签。
    pub is_added_sign: bool,
    /// 是否自审。
    pub is_self_approval: bool,
    /// 是否系统代点。
    pub is_auto: bool,
    /// 状态（`PENDING` / `SKIPPED`）。
    pub status: String,
    /// 意见。
    pub comment: Option<String>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 转办来源行。
    pub transferred_from: Option<Uuid>,
    /// 表达式命中的规则 id（`EXPRESSION` 节点专属，D-E5）；静态来源写 `None`。
    ///
    /// 不写这一列 = 用户/审计只能看到「他是 ROLE 财务总监」，看不到「为什么命中财务总监」。
    /// `assignee_source` 仍然写**底层来源**（`ROLE`/`USER`/…），两者是互补而不是替代。
    pub assignee_rule_id: Option<String>,
    /// 升级留痕：原本该审的角色编码（P3-02；`SKIPPED` 留痕行恒为 `None`）。
    pub escalated_from_role: Option<String>,
    /// 升级原因：`SKIP_SELF_APPROVAL` / `NO_MEMBER`（P3-02；`SKIPPED` 留痕行恒为 `None`）。
    pub escalate_reason: Option<String>,
    /// 派单规则留痕（P3-03）：`ROLE` / `ESCALATED` / `FALLBACK` / `DELEGATED`；`None` = 默认直派。
    pub assigned_by_rule: Option<String>,
}

/// 一次「按业务单据批量终止在跑审批单」的结果（T12）。
///
/// 三张表各被改了多少行 —— 调用方用来判断「到底有没有在跑的审批单」，
/// 并写进结构化日志（验收脚本也按这些数字断言）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CancelledOfBiz {
    /// 被置为 `CANCELLED` 的实例数（`biz_flow_instance`）。
    pub instances: u64,
    /// 被置为 `CANCELLED` 的审批待办数（`biz_flow_task`）。
    pub tasks: u64,
    /// 被置为 `CANCELLED` 的节点数（`biz_flow_instance_node`）。
    pub nodes: u64,
}

/// 被「流程实例走到终态」连带取消的一张流程任务（变更日志与 e2e 断言用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledFlowTask {
    /// 任务 id。
    pub task_id: Uuid,
    /// 任务编号。
    pub task_no: String,
    /// 取消**之前**的状态（`TODO` / `DOING` / `REVIEW`）—— 变更日志的 `from` 必须是旧值。
    pub from: String,
}

/// 留痕挂点：实例上**最新的一个节点行**（审批时间线按 `node_key` 分组，留痕挂在它上面）。
#[derive(Debug, Clone)]
pub struct NodeAnchor {
    /// 实例节点主键（`biz_flow_task.instance_node_id` 的外键目标）。
    pub id: Uuid,
    /// 节点 key（时间线分组键）。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
}

// SCOPE-EXEMPT: 按 `instance_id` 精确取**单条**节点行（`ORDER BY … LIMIT 1`），不是列表查询；
// 调用方是审批回调（系统身份），实例可见性已在发起侧校验，这里再套数据范围只会把留痕弄丢。
/// 新建抄送行入参（参数收成结构体：clippy 的 `too_many_arguments` 阈值是 7）。
#[derive(Debug, Clone)]
pub struct NewCc {
    /// 实例。
    pub instance_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 收件人。
    pub receiver_id: Uuid,
    /// 收件人姓名快照。
    pub receiver_name: Option<String>,
    /// 来源（与 `assignee_source` 同取值域）。
    pub source_type: String,
    /// 来源引用（角色 / 部门 id）。
    pub source_ref: Option<Uuid>,
}

/// 用户简要信息（转办 / 加签校验用）。
#[derive(Debug, Clone)]
pub struct UserBrief {
    /// id。
    pub id: Uuid,
    /// 姓名（`real_name` 为空时用 `username`）。
    pub name: String,
    /// 是否机器人。
    pub is_robot: bool,
    /// 是否启用。
    pub active: bool,
}

pub(super) fn opt_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::from(&id),
        None => Value::Null,
    }
}

pub(super) fn opt_ts(value: Option<chrono::DateTime<chrono::Utc>>) -> Value {
    match value {
        Some(ts) => Value::Text(ts.to_rfc3339()),
        None => Value::Null,
    }
}

// ==================== 流程生成的任务（P-FLOW-1A） ====================

/// 一张「流程生成的任务」行（`GET /business/flow/instance/{id}/tasks` 的出参来源）。
#[derive(Debug, Clone)]
pub struct FlowTaskRow {
    /// 任务 id。
    pub id: Uuid,
    /// 任务编号。
    pub task_no: String,
    /// 标题（生成时渲染的快照）。
    pub title: String,
    /// 状态。
    pub status: String,
    /// 优先级。
    pub priority: String,
    /// 进度。
    pub progress: i16,
    /// 负责人。
    pub owner_id: Uuid,
    /// 负责人姓名（已删用户 → 空串）。
    pub owner_name: String,
    /// 是否需验收。
    pub need_review: bool,
    /// 截止日。
    pub due_date: Option<chrono::NaiveDate>,
    /// 生成它的节点 key。
    pub flow_node_key: String,
    /// 生成时的轮次。
    pub flow_round: i32,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}
