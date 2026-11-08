//! 审批流程图算法（批次 D · D1）：**纯逻辑，不碰数据库**。
//!
//! 依据：`docs/批次D-工作流审批-技术架构.md` §3（流程模型）、§3.7（发布校验 12 条）、
//!       §8（环检测：**应用层 O(V+E) 遍历，严禁递归 CTE**）、§0.1 D20。
//!
//! # 为什么这里没有一行 SQL
//!
//! 架构 §8 的三条实测证据：
//!
//! | 写法 | 200 节点 + 60 跳边的合法 DAG 上的表现 |
//! |------|--------------------------------------|
//! | `WITH RECURSIVE … CYCLE`（路径枚举） | **300 秒未结束**（路径数 ≈ 1.618^60 ≈ 10¹²） |
//! | `WITH RECURSIVE … UNION`（去重） | 200 行、正常结束，但**有环也返回 200 行 ⇒ 判不出环** |
//! | `UNION ALL` + 深度上限 | 跑得完，但环被「截断」而非「识别」 |
//! | **一次读全图 + 内存遍历** | 读图 **0.250 ms**，遍历 O(V+E)=399 步（微秒级） |
//!
//! 所以：`model::load_graph` 一次把节点 / 边 / 审批人读进内存，本模块只做内存算法。
//! 规模上限（[`MAX_NODES`] / [`MAX_EDGES`]）拦在发布期（R15 / `65017`），
//! **遍历本身不设深度上限**（加了上限只会引入「超限就当无环」的假阴性），
//! 但留了一个迭代次数保险（`nodes.len() * 2 + edges.len() * 2 + 8`）防代码 bug 死循环。
//!
//! # 发布校验清单
//!
//! [`FlowGraph::validate`] 按卡面顺序 R1–R12，另外补上架构 §3.7 文档里的 R13（条件节点缺兜底分支）、
//! R14（节点未配置审批人）、R15（规模上限）、R16（`EXPRESSION` 表达式可解析 / 不引用保留词）——
//! 卡面的 12 条与文档的 V1–V12 是同一套语义的两种切法，
//! 映射表见 `docs/批次D-D1-验证证据.md` 与每个 `rule` 的文档注释。
//!
//! # 环路径必须「点名」
//!
//! 架构 §3.7 的硬性要求（与 T4 的 `dep::find_cycle` 同款）：不许只回「有环」两个字，
//! 必须给出**闭合路径**（首尾相同），前端才画得出来：`n_legal → n_finance → n_review → n_legal`。

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use nik::uuid::Uuid;

use crate::business::flow::expression;
use crate::business::flow::node_config;
use crate::business::notice::render;
use crate::shared::error_code;

// ==================== 规模上限（R15 / 65017） ====================

/// 节点数上限（架构 §3.7 V12）。
pub const MAX_NODES: usize = 200;
/// 边数上限（架构 §3.7 V12）。
pub const MAX_EDGES: usize = 400;

// ==================== 条件 DSL 上限（R10 / 65016） ====================

/// 条件表达式树深上限（架构 §3.4.1）。
pub const MAX_CONDITION_DEPTH: usize = 8;
/// 条件表达式总节点数上限。
pub const MAX_CONDITION_NODES: usize = 64;
/// 单个 `and`/`or` 的 `args` 上限。
pub const MAX_CONDITION_ARGS: usize = 16;
/// 字符串字面量长度上限（字符数）。
pub const MAX_LITERAL_CHARS: usize = 200;
/// 数组字面量元素上限。
pub const MAX_LITERAL_ARRAY: usize = 50;

// ==================== 七种节点 ====================

/// 七种节点类型（架构 §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeType {
    /// 开始（恰 1 个，入度 0，出度 1）。
    Start,
    /// 单签审批（出度必须 = 1）。
    Approval,
    /// 会签（出度必须 = 1，`approval_mode` 必填）。
    Countersign,
    /// 条件分支（出度 ≥ 2，每条出边带条件 + 恰一条 default）。
    Condition,
    /// 并行网关（出度 ≥ 2，按 `fork_mode` 激活）。
    Parallel,
    /// 抄送（非阻塞，出度必须 = 1）。
    Cc,
    /// 任务节点（P-FLOW-1A）：阻塞等任务完成，生成 `biz_task`（出度必须 = 1）。
    ///
    /// 与审批类的区别是**生成物不同**：审批类生成 `biz_flow_task`（要你签字），
    /// 本类型生成 `biz_task`（要你干活，有进度 / 工时 / 验收 / 子任务）。
    /// 混成一体会让「审批中心」和「工作任务」两个入口都变脏（设计 §1.2 / §9.1）。
    Task,
    /// 通知节点（P-FLOW-2）：非阻塞，生成 `sys_notice`（出度必须 = 1）。
    ///
    /// 引擎分支见 `engine::step` 的 `NodeType::Notify` 臂：进入即解析收件人 + 渲染模板 +
    /// 产生一条 `NoticeDraft` 副作用，然后节点立即 `APPROVED` 驱动下游；
    /// `when=ON_TIMEOUT` 时节点保持 `ACTIVE`，由 `engine::fire_notify_node` 在超时后放行。
    /// 发布校验见 `FlowGraph::validate_with` 的 R19。
    Notify,
    /// 子流程节点（P3-08 / ARCH-5 §4.14）：一个节点里跑**另一张完整的图**（出度必须 = 1）。
    ///
    /// # 与「展开子流程」的区别（为什么是真子实例）
    ///
    /// 把子流程的节点内联展开进父图会撞 `node_key`（只能加前缀），而前缀一加，
    /// 子流程改版就得重新展开所有父流程 —— 子流程自己的审批记录、超时、统计也全部混进
    /// 父实例，没人能说清「这张单到底卡在哪」。所以落地方式是**真子实例**：
    /// 父实例的 SUBPROCESS 节点保持 `ACTIVE`，子实例跑自己的图，结算时回调父节点。
    ///
    /// # 三条运行期约束
    ///
    /// ① `biz_flow_node.subprocess_def_id` 必填且必须已发布（R27，`65045`）；
    /// ② 嵌套深度 ≤ [`crate::business::flow::subprocess::MAX_NEST_DEPTH`]（R27，`65046`）；
    /// ③ 子实例 `biz_id = NULL` —— 不撞父流程的 `uk_biz_flow_instance_biz_active`
    ///    （同单据同时只能一个在跑）。
    Subprocess,
    /// 结束（≥ 1 个，出度 0）。
    End,
}

impl NodeType {
    /// 入库 / 出参字符串（与 `biz_flow_node.node_type` 的 CHECK 逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            NodeType::Start => "START",
            NodeType::Approval => "APPROVAL",
            NodeType::Countersign => "COUNTERSIGN",
            NodeType::Condition => "CONDITION",
            NodeType::Parallel => "PARALLEL",
            NodeType::Cc => "CC",
            NodeType::Task => "TASK_NODE",
            NodeType::Notify => "NOTIFY_NODE",
            NodeType::Subprocess => "SUBPROCESS",
            NodeType::End => "END",
        }
    }

    /// 解析库里的字符串（大小写不敏感；未知值 → `None`，上层给中文错误）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "START" => Some(NodeType::Start),
            "APPROVAL" => Some(NodeType::Approval),
            "COUNTERSIGN" => Some(NodeType::Countersign),
            "CONDITION" => Some(NodeType::Condition),
            "PARALLEL" => Some(NodeType::Parallel),
            "CC" => Some(NodeType::Cc),
            "TASK_NODE" => Some(NodeType::Task),
            "NOTIFY_NODE" => Some(NodeType::Notify),
            "SUBPROCESS" => Some(NodeType::Subprocess),
            "END" => Some(NodeType::End),
            _ => None,
        }
    }

    /// 全部节点类型（入库字符串，顺序与 `as_str` 一致；错误文案拼「只允许 …」用）。
    pub const ALL: [NodeType; 10] = [
        NodeType::Start,
        NodeType::Approval,
        NodeType::Countersign,
        NodeType::Condition,
        NodeType::Parallel,
        NodeType::Cc,
        NodeType::Task,
        NodeType::Notify,
        NodeType::Subprocess,
        NodeType::End,
    ];

    /// 是否「审批类」节点（出度必须 = 1，架构 D7：只有显式网关能分叉）。
    ///
    /// # 为什么 `TASK_NODE` / `NOTIFY_NODE` **不在**这里
    ///
    /// 它们同样受「出度必须 = 1」约束（设计 §2.7 R17），但**语义不同**：
    /// `is_approval_class()` 的调用方（R6）文案是「审批节点只能有一条出边」，
    /// 把任务/通知节点塞进来会让用户看到一句错误的解释。
    /// 「出度 = 1」这种结构属性统一走 [`NodeType::is_single_exit`]；
    /// 「是审批语义」才用本方法。两条约束各有各的规则编号（R6 / R17），互不覆盖。
    pub fn is_approval_class(self) -> bool {
        matches!(
            self,
            NodeType::Approval | NodeType::Countersign | NodeType::Cc
        )
    }

    /// 出度必须 = 1 的节点（审批类 + 任务 + 通知 + 子流程；R6 / R17 / R27 的共同判据）。
    pub fn is_single_exit(self) -> bool {
        self.is_approval_class()
            || matches!(
                self,
                NodeType::Task | NodeType::Notify | NodeType::Subprocess
            )
    }

    /// 是否分支网关（条件 / 并行，出度 ≥ 2）。
    pub fn is_gateway(self) -> bool {
        matches!(self, NodeType::Condition | NodeType::Parallel)
    }

    /// 是否必须配置「指派来源」（R14 审批人 / R20 任务负责人）。
    ///
    /// # `TASK_NODE` 为什么也算
    ///
    /// 派不出人 = 静默丢活（设计 §2.4）：审批节点派不出人只是少一个签字（下游还会审），
    /// 任务节点派不出人就是「这个活没人干、流程还继续往下」。所以两者都必须有来源，
    /// 只是与 R14 共用同一段判定、互相**不重复报错**（见 `FlowGraph::check_assignees`：
    /// 它按节点类型分派规则编号与用词）。
    pub fn needs_assignee(self) -> bool {
        matches!(
            self,
            NodeType::Approval | NodeType::Countersign | NodeType::Task
        )
    }
}

/// 签名模式（架构 D3）。`approval_mode` 对 `APPROVAL` 与 `COUNTERSIGN` 都生效（P3-06）。
///
/// # 两代语义（P3-06 之后）
///
/// - `COUNTERSIGN`：**必须显式**给值（R9 拦），三种模式都允许；
/// - `APPROVAL`：**可以不给**（`NULL` 等价 `Any` = 或签）。要全员签必须显式写 `ALL`。
///
/// 引擎侧的唯一入口是 [`ApprovalMode::effective`]：`NULL` / 空串 / 非法值一律落到 `Any`。
/// 存量节点由 `V0339` 按「解析出的候选人数」冻结（≥2 写 `ALL` 保持原行为，<2 写 `ANY`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalMode {
    /// 全员同意（会签）。
    All,
    /// 按比例（阈值见 `approve_ratio`）。
    Ratio,
    /// 或签（任一同意）。
    Any,
}

impl ApprovalMode {
    /// 库里的 `approval_mode` 文本 → 生效模式（**引擎唯一兜底口径**，P3-06）。
    ///
    /// # 为什么兜底是 `Any` 而不是 `All`
    ///
    /// `NULL` 的语义是「没特别说明」，默认不该比用户写明的要求更严：
    /// 一个 `APPROVAL` 节点派给 3 个人时，写「或签」= 任意一人处理即可（审批要的是
    /// 「有人负责」，不是「所有人都必须点一遍」）；要全员签请显式选 `ALL`。
    ///
    /// 非法值（理论不可能 —— 库里有 `CHECK (approval_mode IN ('ALL','RATIO','ANY'))`）
    /// 也落 `Any`：宁可放宽也不要让一个错字把流程卡死在「等全员」上。
    pub fn effective(raw: Option<&str>) -> Self {
        raw.and_then(ApprovalMode::parse)
            .unwrap_or(ApprovalMode::Any)
    }

    /// 入库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::All => "ALL",
            ApprovalMode::Ratio => "RATIO",
            ApprovalMode::Any => "ANY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ALL" => Some(ApprovalMode::All),
            "RATIO" => Some(ApprovalMode::Ratio),
            "ANY" => Some(ApprovalMode::Any),
            _ => None,
        }
    }
}

/// 存量冻结口径（P3-06 D2）：给定「节点解析出的候选人数」→ 该写什么 `approval_mode`。
///
/// # 规则（与 `V0339` 的 SQL 逐字同义）
///
/// - `Some(n)` 且 `n >= 2` → [`ApprovalMode::All`]：**保持当前行为**（历史语义是
///   「`NULL` 按全员签跑」），不动在跑流程的签法；
/// - `Some(0)` / `Some(1)` → [`ApprovalMode::Any`]：`ALL` 与 `ANY` 在 0/1 人时**判定等价**
///   （0 人自动通过、1 人签完即通过），写 `ANY` 只是把隐式语义显式化；
/// - `None`（算不出来：`FORM_VAR`/`EXPRESSION`/没配来源等）→ [`ApprovalMode::Any`]：
///   同样走「不填=或签」的新语义，不猜一个更严的值。
///
/// 纯函数，不碰库；SQL 侧的同一份口径见 `V0339__approval_sign_mode_freeze.up.sql`。
pub fn freeze_sign_mode(candidates: Option<usize>) -> ApprovalMode {
    match candidates {
        Some(n) if n >= 2 => ApprovalMode::All,
        _ => ApprovalMode::Any,
    }
}

/// 网关模式：`fork_mode`（PARALLEL 分叉）与 `join_mode`（汇聚）共用取值域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayMode {
    /// 全部（AND）。
    And,
    /// 任一（OR）。
    Or,
}

impl GatewayMode {
    /// 入库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            GatewayMode::And => "AND",
            GatewayMode::Or => "OR",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "AND" => Some(GatewayMode::And),
            "OR" => Some(GatewayMode::Or),
            _ => None,
        }
    }
}

/// 审批人来源（架构 §4.3 的 8 种来源 + ADMIN + 批次 D 的 `EXPRESSION`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssigneeKind {
    /// 指定用户。
    User,
    /// 指定角色。
    Role,
    /// 发起人本人。
    Initiator,
    /// 发起人主管（`superior_levels` 层）。
    InitiatorSuperior,
    /// 部门负责人。
    DeptLeader,
    /// 任务负责人。
    TaskOwner,
    /// 任务创建人。
    TaskCreator,
    /// 任务分派人。
    TaskAssigner,
    /// 表单变量（`form_var_key`）。
    FormVar,
    /// 平台管理员。
    Admin,
    /// 动态表达式（批次 D · EXPRESSION DSL）：规则树存 `biz_flow_node.config.expression`，
    /// 运行期求值后展开成上面 10 种来源之一（见 `business::flow::expression`）。
    Expression,
}

impl AssigneeKind {
    /// 入库字符串（与 `biz_flow_node.assignee_type` 的 CHECK 逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            AssigneeKind::User => "USER",
            AssigneeKind::Role => "ROLE",
            AssigneeKind::Initiator => "INITIATOR",
            AssigneeKind::InitiatorSuperior => "INITIATOR_SUPERIOR",
            AssigneeKind::DeptLeader => "DEPT_LEADER",
            AssigneeKind::TaskOwner => "TASK_OWNER",
            AssigneeKind::TaskCreator => "TASK_CREATOR",
            AssigneeKind::TaskAssigner => "TASK_ASSIGNER",
            AssigneeKind::FormVar => "FORM_VAR",
            AssigneeKind::Admin => "ADMIN",
            AssigneeKind::Expression => "EXPRESSION",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "USER" => Some(AssigneeKind::User),
            "ROLE" => Some(AssigneeKind::Role),
            "INITIATOR" => Some(AssigneeKind::Initiator),
            "INITIATOR_SUPERIOR" => Some(AssigneeKind::InitiatorSuperior),
            "DEPT_LEADER" => Some(AssigneeKind::DeptLeader),
            "TASK_OWNER" => Some(AssigneeKind::TaskOwner),
            "TASK_CREATOR" => Some(AssigneeKind::TaskCreator),
            "TASK_ASSIGNER" => Some(AssigneeKind::TaskAssigner),
            "FORM_VAR" => Some(AssigneeKind::FormVar),
            "ADMIN" => Some(AssigneeKind::Admin),
            "EXPRESSION" => Some(AssigneeKind::Expression),
            _ => None,
        }
    }

    /// 是否「可枚举」来源 —— 只有这三种能落 `biz_flow_node_assignee` 行（架构 §6.1 注）。
    ///
    /// `EXPRESSION` **不**可枚举：它没有固定的审批人清单，具体目标由规则树在运行期算出
    /// （否则 R14 会误报「一个审批人都没选」，见 EXPRESSION-DSL 规范 §8.7 第 2 条）。
    pub fn is_enumerable(self) -> bool {
        matches!(
            self,
            AssigneeKind::User | AssigneeKind::Role | AssigneeKind::DeptLeader
        )
    }
}

// ==================== 图元素 ====================

/// 节点审批人引用（可枚举来源）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssigneeRef {
    /// `USER` / `ROLE` / `DEPT_LEADER`。
    pub kind: String,
    /// 目标 id（`sys_user` / `sys_role` / `sys_dept` 的主键）。
    pub ref_id: String,
    /// 展示顺序。
    pub sort_order: i32,
}

/// 编辑期节点（[`crate::business::flow::model::NodeRow`] 的纯逻辑镜像）。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSpec {
    /// 节点 key（版本内唯一，`^[A-Za-z][A-Za-z0-9_]{0,63}$`）。
    pub key: String,
    /// 展示名。
    pub name: String,
    /// 节点类型。
    pub node_type: NodeType,
    /// 审批人来源。
    pub assignee_type: Option<AssigneeKind>,
    /// 签名模式（`APPROVAL` + `COUNTERSIGN`；`APPROVAL` 允许 `None` = 或签，P3-06）。
    pub approval_mode: Option<ApprovalMode>,
    /// 会签阈值（仅 RATIO）。
    pub approve_ratio: Option<f64>,
    /// 一票否决（与 `approval_mode` 正交，架构 D4）。
    pub reject_veto: bool,
    /// 汇聚模式。
    pub join_mode: GatewayMode,
    /// 分叉模式。
    pub fork_mode: GatewayMode,
    /// `FORM_VAR` 来源的变量名。
    pub form_var_key: Option<String>,
    /// 主管层数（`INITIATOR_SUPERIOR`）。
    pub superior_levels: i16,
    /// 可枚举审批人。
    pub assignees: Vec<AssigneeRef>,
    /// 空审批人策略（`SKIP` / `ERROR` / `FALLBACK_ADMIN`，架构 X2）。
    pub empty_assignee: String,
    /// 自审策略（`ALLOW` / `SKIP` / `FORBID`，架构 X3）。
    pub self_approval: String,
    /// 是否允许机器人审批（架构 D18：隐式来源默认过滤 ROBOT）。
    pub allow_robot: bool,
    /// 兜底审批人（用户 id，P3-03）：升级链走完仍无人时由他接管。
    ///
    /// 与 [`Self::fallback_role_id`] **二选一**（DB 有 CHECK，发布期 R22 再挡一次）；
    /// 两个都 `None` = 不兜底（与 P3-02 之后的行为逐字一致）。
    pub fallback_user_id: Option<Uuid>,
    /// 兜底角色（角色 id，P3-03）：升级链走完仍无人时展开该角色成员。
    pub fallback_role_id: Option<Uuid>,
    /// 超时小时（`None` = 不超时）。
    pub timeout_hours: Option<i32>,
    /// 超时动作（`REMIND` / `AUTO_APPROVE` / `AUTO_REJECT` / `ESCALATE`，架构 D10）。
    pub timeout_action: String,
    /// 提前多少小时提醒。
    pub remind_before_hours: Option<i32>,
    /// 最多提醒次数。
    pub max_remind_times: i16,
    /// 通过后是否抄送。
    pub cc_on_approved: bool,
    /// 画布 x。
    pub x: i32,
    /// 画布 y。
    pub y: i32,
    /// 排序。
    pub sort_order: i32,
    /// 子流程定义（仅 `SUBPROCESS`；P3-08 / ARCH-5 §4.14）。
    ///
    /// 对应表列 `biz_flow_node.subprocess_def_id`（FK RESTRICT 到 `biz_flow_def`）。
    /// 用**物理列**而不是 `config.sub.def_id` 的三个理由：
    /// ① 外键约束只在列上生效（JSONB 里的 uuid 没人拦得住「删了定义父流程还指着它」）；
    /// ② 「哪些流程引用了这个子流程定义」要能反查（`idx_biz_flow_node_subprocess_def`）；
    /// ③ `config` 整块进 `graph_hash`，uuid 放列上还能让「只换子流程版本」不改 hash。
    pub subprocess_def_id: Option<Uuid>,
    /// 扩展配置（JSONB，**不含审批人**）。
    pub config: Value,
}

impl Default for NodeSpec {
    fn default() -> Self {
        Self {
            key: String::new(),
            name: String::new(),
            node_type: NodeType::Start,
            assignee_type: None,
            approval_mode: None,
            approve_ratio: None,
            reject_veto: true,
            join_mode: GatewayMode::And,
            fork_mode: GatewayMode::And,
            form_var_key: None,
            superior_levels: 1,
            assignees: Vec::new(),
            empty_assignee: "SKIP".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 0,
            subprocess_def_id: None,
            config: Value::Object(Map::new()),
        }
    }
}

impl NodeSpec {
    /// 测试 / 建默认节点用：`NodeSpec::new("n_start", "开始", NodeType::Start)`。
    pub fn new(key: impl Into<String>, name: impl Into<String>, node_type: NodeType) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            node_type,
            ..Self::default()
        }
    }
}

/// 编辑期连线。
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeSpec {
    /// 边 key（版本内唯一）。
    pub key: String,
    /// 起点 node key。
    pub from: String,
    /// 终点 node key。
    pub to: String,
    /// 条件 DSL（`None` = 无条件边）。
    pub condition: Option<Value>,
    /// 条件标签（展示用）。
    pub label: Option<String>,
    /// 优先级（条件求值顺序）。
    pub priority: i32,
    /// 是否兜底边（仅 CONDITION 节点，恰一条）。
    pub is_default: bool,
}

impl EdgeSpec {
    /// 构造一条无条件边。
    pub fn new(key: impl Into<String>, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            from: from.into(),
            to: to.into(),
            condition: None,
            label: None,
            priority: 0,
            is_default: false,
        }
    }
}

/// 一张流程图（节点 + 边）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowGraph {
    /// 节点（顺序即入库顺序，不影响算法结果）。
    pub nodes: Vec<NodeSpec>,
    /// 边。
    pub edges: Vec<EdgeSpec>,
}

// ==================== 图算法 ====================

/// 出边邻接表（`from key → 出边列表`，保持 `edges` 的原始顺序 ⇒ 算法结果可复现）。
type OutMap<'a> = HashMap<&'a str, Vec<&'a EdgeSpec>>;

impl FlowGraph {
    /// 按 key 取节点。
    pub fn node(&self, key: &str) -> Option<&NodeSpec> {
        self.nodes.iter().find(|node| node.key == key)
    }

    /// 出边邻接表。**只收两端都存在的边**（悬空边由保存期校验拦，见 `handler::save_graph`）。
    pub fn out_edges(&self) -> OutMap<'_> {
        let mut map: OutMap<'_> = HashMap::with_capacity(self.nodes.len());
        for edge in &self.edges {
            if self.node(&edge.from).is_some() && self.node(&edge.to).is_some() {
                map.entry(edge.from.as_str()).or_default().push(edge);
            }
        }
        map
    }

    /// 入边邻接表。
    pub fn in_edges(&self) -> OutMap<'_> {
        let mut map: OutMap<'_> = HashMap::with_capacity(self.nodes.len());
        for edge in &self.edges {
            if self.node(&edge.from).is_some() && self.node(&edge.to).is_some() {
                map.entry(edge.to.as_str()).or_default().push(edge);
            }
        }
        map
    }

    /// 入度表（只算两端都存在的边）。
    pub fn in_degree(&self) -> HashMap<&str, usize> {
        let mut map: HashMap<&str, usize> = self
            .nodes
            .iter()
            .map(|node| (node.key.as_str(), 0))
            .collect();
        for edge in &self.edges {
            if self.node(&edge.from).is_none() {
                continue;
            }
            if let Some(slot) = map.get_mut(edge.to.as_str()) {
                *slot += 1;
            }
        }
        map
    }

    /// 出度表（只算两端都存在的边）。
    pub fn out_degree(&self) -> HashMap<&str, usize> {
        let mut map: HashMap<&str, usize> = self
            .nodes
            .iter()
            .map(|node| (node.key.as_str(), 0))
            .collect();
        for edge in &self.edges {
            if self.node(&edge.to).is_none() {
                continue;
            }
            if let Some(slot) = map.get_mut(edge.from.as_str()) {
                *slot += 1;
            }
        }
        map
    }

    /// 环检测：**应用层 O(V+E) DFS**（`visited` + `in_stack`），返回**闭合路径**（首尾相同）。
    ///
    /// # 为什么是迭代而不是递归
    ///
    /// 200 节点的链在递归实现下会把调用栈压到 200 层；迭代写法把路径放在显式 `Vec` 里，
    /// 既没有栈溢出风险，也能在检出回边时**直接切出闭合路径**（递归版要额外维护 parent 链）。
    ///
    /// # 迭代次数保险
    ///
    /// 每弹一次栈帧都计数，超过 `nodes*2 + edges*2 + 8` 即返回 `None`（防代码 bug 死循环）。
    /// 正常图上步数 = O(V+E)，永远碰不到这个上限（架构 §8.3）。
    ///
    /// # 示例
    ///
    /// ```
    /// use cloudpivot::business::flow::graph::{EdgeSpec, FlowGraph, NodeSpec, NodeType};
    ///
    /// let mut graph = FlowGraph::default();
    /// graph.nodes = vec![
    ///     NodeSpec::new("a", "A", NodeType::Start),
    ///     NodeSpec::new("b", "B", NodeType::Approval),
    ///     NodeSpec::new("c", "C", NodeType::End),
    /// ];
    /// graph.edges = vec![
    ///     EdgeSpec::new("e1", "a", "b"),
    ///     EdgeSpec::new("e2", "b", "c"),
    ///     EdgeSpec::new("e3", "c", "b"), // 回边
    /// ];
    /// let cycle = graph.find_cycle().expect("必须检出环");
    /// assert_eq!(cycle.first(), cycle.last());
    /// ```
    pub fn find_cycle(&self) -> Option<Vec<String>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Colour {
            White,
            Gray,
            Black,
        }

        let out = self.out_edges();
        let budget = self.nodes.len() * 2 + self.edges.len() * 2 + 8;
        let mut steps = 0usize;
        let mut colour: HashMap<&str, Colour> = self
            .nodes
            .iter()
            .map(|n| (n.key.as_str(), Colour::White))
            .collect();

        for root in &self.nodes {
            if colour.get(root.key.as_str()) != Some(&Colour::White) {
                continue;
            }
            // 显式 DFS 栈：`(节点 key, 下一条待访问出边下标)`
            let mut stack: Vec<(&str, usize)> = vec![(root.key.as_str(), 0)];
            // 当前深搜路径（栈里的 key 序列，全部是 Gray）
            let mut path: Vec<&str> = vec![root.key.as_str()];
            colour.insert(root.key.as_str(), Colour::Gray);

            while let Some((current, next_index)) = stack.pop() {
                steps += 1;
                if steps > budget {
                    tracing::warn!(
                        nodes = self.nodes.len(),
                        edges = self.edges.len(),
                        "环检测迭代次数超过保险阈值，按无环返回"
                    );
                    return None;
                }
                let children = out.get(current).map(Vec::as_slice).unwrap_or(&[]);
                if next_index >= children.len() {
                    // 该节点的出边走完了：出栈并把它从路径里摘掉（Gray → Black）
                    colour.insert(current, Colour::Black);
                    if path.last() == Some(&current) {
                        path.pop();
                    }
                    continue;
                }
                stack.push((current, next_index + 1));
                let next = children[next_index].to.as_str();
                match colour.get(next).copied().unwrap_or(Colour::White) {
                    Colour::Gray => {
                        // 回边：路径上从 next 开始的那一段 + next 就是闭合路径
                        let pos = path.iter().position(|key| *key == next).unwrap_or(0);
                        let mut cycle: Vec<String> =
                            path[pos..].iter().map(|key| (*key).to_string()).collect();
                        cycle.push(next.to_string());
                        return Some(cycle);
                    }
                    Colour::White => {
                        colour.insert(next, Colour::Gray);
                        stack.push((next, 0));
                        path.push(next);
                    }
                    Colour::Black => {}
                }
            }
        }
        None
    }

    /// 拓扑排序（Kahn 算法）。有环 → `None`。
    ///
    /// 与 [`Self::find_cycle`] 是**两套等价实现**：Kahn 顺带给出「剩下的节点都在环上」，
    /// 用于交叉校验（发布校验只跑 DFS，Kahn 主要给 D2 的引擎做节点激活顺序参考）。
    pub fn topological_order(&self) -> Option<Vec<String>> {
        let out = self.out_edges();
        let mut indegree: HashMap<&str, usize> = self.in_degree();
        // 入度 0 的按 nodes 原顺序入队（结果稳定，便于断言）
        let mut queue: VecDeque<&str> = self
            .nodes
            .iter()
            .filter(|node| indegree.get(node.key.as_str()) == Some(&0))
            .map(|node| node.key.as_str())
            .collect();
        let mut order: Vec<String> = Vec::with_capacity(self.nodes.len());
        while let Some(key) = queue.pop_front() {
            order.push(key.to_string());
            for edge in out.get(key).map(Vec::as_slice).unwrap_or(&[]) {
                if let Some(slot) = indegree.get_mut(edge.to.as_str()) {
                    *slot -= 1;
                    if *slot == 0 {
                        queue.push_back(edge.to.as_str());
                    }
                }
            }
        }
        if order.len() == self.nodes.len() {
            Some(order)
        } else {
            None
        }
    }

    /// 环上节点（Kahn 摘不掉的节点）。无环 → 空。
    ///
    /// 与 [`Self::find_cycle`] 的路径互补：报错文案要「点名环上每个节点」（架构 §3.7 V3）。
    pub fn cycle_nodes(&self) -> Vec<String> {
        let out = self.out_edges();
        let mut indegree: HashMap<&str, usize> = self.in_degree();
        let mut queue: VecDeque<&str> = self
            .nodes
            .iter()
            .filter(|node| indegree.get(node.key.as_str()) == Some(&0))
            .map(|node| node.key.as_str())
            .collect();
        let mut removed: HashSet<&str> = HashSet::new();
        while let Some(key) = queue.pop_front() {
            removed.insert(key);
            for edge in out.get(key).map(Vec::as_slice).unwrap_or(&[]) {
                if let Some(slot) = indegree.get_mut(edge.to.as_str()) {
                    *slot -= 1;
                    if *slot == 0 {
                        queue.push_back(edge.to.as_str());
                    }
                }
            }
        }
        self.nodes
            .iter()
            .filter(|node| !removed.contains(node.key.as_str()))
            .map(|node| node.key.clone())
            .collect()
    }

    /// 从给定起点沿出边 BFS 能到达的节点集合（含起点自身）。
    pub fn reach_from(&self, starts: &[String]) -> HashSet<String> {
        let out = self.out_edges();
        let mut seen: HashSet<String> = starts.iter().cloned().collect();
        let mut queue: VecDeque<String> = starts.iter().cloned().collect();
        let mut steps = 0usize;
        let budget = self.nodes.len() * 2 + self.edges.len() * 2 + 8;
        while let Some(key) = queue.pop_front() {
            steps += 1;
            if steps > budget {
                break;
            }
            for edge in out.get(key.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }
        seen
    }

    /// 能沿出边走到给定终点集合的节点（反向 BFS，含终点自身）。
    pub fn reach_to(&self, targets: &[String]) -> HashSet<String> {
        let input = self.in_edges();
        let mut seen: HashSet<String> = targets.iter().cloned().collect();
        let mut queue: VecDeque<String> = targets.iter().cloned().collect();
        let mut steps = 0usize;
        let budget = self.nodes.len() * 2 + self.edges.len() * 2 + 8;
        while let Some(key) = queue.pop_front() {
            steps += 1;
            if steps > budget {
                break;
            }
            for edge in input.get(key.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(edge.from.clone()) {
                    queue.push_back(edge.from.clone());
                }
            }
        }
        seen
    }

    /// 起点集合（全部 START 节点），按 nodes 顺序。
    pub fn start_keys(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|node| node.node_type == NodeType::Start)
            .map(|node| node.key.clone())
            .collect()
    }

    /// 终点集合（全部 END 节点），按 nodes 顺序。
    pub fn end_keys(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|node| node.node_type == NodeType::End)
            .map(|node| node.key.clone())
            .collect()
    }

    /// 从某个节点反向找最近的「分支网关」（`CONDITION` / `PARALLEL`）。
    ///
    /// 用于 R8（禁止混流汇聚，架构 §3.5.3）：汇聚节点的每条入边都要能回答
    /// 「我是从哪个网关分出来的」。BFS 保证找到的是**最近**的那个（嵌套网关时以最近为准）。
    /// 找不到（例如整条链上根本没有网关，却有两条入边 —— 其实只能是图上另有环路）→ `None`。
    ///
    /// # `from_key` 自身就是网关时**必须返回它自己**
    ///
    /// 「有则加一道复核、无则直过」（BPMN 的 exclusive gateway + 旁路）在图上长这样：
    ///
    /// ```text
    ///   c_cond ──条件成立──▶ n_extra ──▶ n_merge
    ///     └────────兜底（直过）───────▶ n_merge
    /// ```
    ///
    /// `n_merge` 的一条入边**直接来自网关**。若只往上游找，这条边会一路走到 START
    /// 而返 `None`，R8 于是把「一支加了审批、另一支直接回来」判成
    /// 「入边 c_cond 上游没有分支网关」（假阳性），而这条边本身恰恰就带着分支身份。
    /// 反过来，「两条无网关来源的入边真的混流」这种要拦的情况不受影响 —— 那两个来源
    /// 都不是网关，也不会被这里短路。
    pub fn nearest_upstream_fork(&self, from_key: &str) -> Option<(String, NodeType)> {
        if let Some(node) = self
            .node(from_key)
            .filter(|node| node.node_type.is_gateway())
        {
            return Some((node.key.clone(), node.node_type));
        }
        let input = self.in_edges();
        let mut seen: HashSet<&str> = HashSet::from([from_key]);
        let mut queue: VecDeque<&str> = VecDeque::from([from_key]);
        let mut steps = 0usize;
        let budget = self.nodes.len() * 2 + self.edges.len() * 2 + 8;
        while let Some(key) = queue.pop_front() {
            steps += 1;
            if steps > budget {
                return None;
            }
            for edge in input.get(key).map(Vec::as_slice).unwrap_or(&[]) {
                if let Some(node) = self
                    .node(&edge.from)
                    .filter(|node| node.node_type.is_gateway())
                {
                    return Some((node.key.clone(), node.node_type));
                }
                if seen.insert(edge.from.as_str()) {
                    queue.push_back(edge.from.as_str());
                }
            }
        }
        None
    }
}

// ==================== 校验结果 ====================

/// 一条发布校验不通过项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// 规则编号（`R1`–`R16`），与卡面 / 架构 §3.7 的映射见模块文档。
    pub rule: &'static str,
    /// 业务错误码（`crate::shared::error_code::FLOW_*`）。
    pub code: i32,
    /// 面向用户的中文文案（**必须点名节点 / 边**，架构 §3.7 V3 的硬性要求）。
    pub message: String,
    /// 涉及的节点 key（前端高亮用）。
    pub nodes: Vec<String>,
}

impl Issue {
    /// 构造一条不通过项。
    pub fn new(rule: &'static str, code: i32, message: impl Into<String>) -> Self {
        Self {
            rule,
            code,
            message: message.into(),
            nodes: Vec::new(),
        }
    }

    /// 带节点 key。
    pub fn with_nodes(mut self, nodes: Vec<String>) -> Self {
        self.nodes = nodes;
        self
    }
}

// ==================== 12 条发布校验（+ 3 条文档补充） ====================

impl FlowGraph {
    /// 全部发布校验（R1–R16），返回**所有**不通过项（不是遇到第一个就返回）。
    ///
    /// 这是 [`FlowGraph::validate_with`] 在 `biz_type = None` 时的入口。
    ///
    /// 为什么收集全部：前端编辑器要把所有问题一次性标在图上（架构 §11.6「前端只做提示，
    /// 后端才是权威」），逐条返回会让用户改一次跑一次。
    pub fn validate(&self) -> Vec<Issue> {
        self.validate_with(None)
    }

    /// 全部发布校验（R1–R21），`biz_type` 只影响 R21 的 `task.*` 判定。
    ///
    /// # `biz_type` 为什么是参数而不是字段
    ///
    /// `FlowGraph` 是「节点 + 边」的纯结构，多了 `biz_type` 会污染 `PartialEq` /
    /// `Default`，还会让所有 `FlowGraph { nodes, edges }` 的构造点（含 60 多条存量单测）
    /// 一起改动。而 R21 只在这一处需要它，作为入参最省事。
    ///
    /// `None` = 不知道业务类型（编辑器「只校验不保存」的旧调用点）：此时**不放行** `task.*`
    /// —— 宁可在保存期多提一次醒，也不要让 `{{task.title}}` 混进非 `TASK_ACCEPT` 流程、
    /// 等到运行期才 422（设计 §2.5.1 的原话：「事到临头才报错的配置等于没校验」）。
    pub fn validate_with(&self, biz_type: Option<&str>) -> Vec<Issue> {
        let mut issues: Vec<Issue> = Vec::new();

        // ---------- R1：恰好 1 个 START（架构 V1） ----------
        let starts = self.start_keys();
        if starts.len() != 1 {
            issues.push(
                Issue::new(
                    "R1",
                    error_code::FLOW_GRAPH_STRUCTURE,
                    format!(
                        "流程必须有且仅有一个开始节点（当前 {} 个{}）",
                        starts.len(),
                        if starts.is_empty() {
                            String::new()
                        } else {
                            format!("：{}", starts.join("、"))
                        }
                    ),
                )
                .with_nodes(starts.clone()),
            );
        }

        // ---------- R2：至少 1 个 END（架构 V2 的一半） ----------
        let ends = self.end_keys();
        if ends.is_empty() {
            issues.push(Issue::new(
                "R2",
                error_code::FLOW_GRAPH_STRUCTURE,
                "流程至少要有一个结束节点",
            ));
        }

        // ---------- R3：无环（架构 V3，DFS 闭合路径 + 点名环上每个节点） ----------
        if let Some(cycle) = self.find_cycle() {
            let path_text = cycle.join(" → ");
            issues.push(
                Issue::new(
                    "R3",
                    error_code::FLOW_GRAPH_CYCLE,
                    format!(
                        "检测到环：{path_text}（流程必须无环；分支请用「条件」节点，并让所有分支最终汇合）"
                    ),
                )
                .with_nodes(cycle),
            );
        }

        // ---------- R4：START 入度 = 0、END 出度 = 0 ----------
        let in_degree = self.in_degree();
        let out_degree = self.out_degree();
        for node in &self.nodes {
            let indegree = in_degree.get(node.key.as_str()).copied().unwrap_or(0);
            let outdegree = out_degree.get(node.key.as_str()).copied().unwrap_or(0);
            if node.node_type == NodeType::Start && indegree != 0 {
                issues.push(
                    Issue::new(
                        "R4",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "开始节点 {} 有 {indegree} 条入边（开始节点必须没有入边）",
                            node.key
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
            if node.node_type == NodeType::End && outdegree != 0 {
                issues.push(
                    Issue::new(
                        "R4",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "结束节点 {} 有 {outdegree} 条出边（结束节点必须没有出边）",
                            node.key
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R5：除 END 外每个节点至少 1 条出边 ----------
        for node in &self.nodes {
            if node.node_type == NodeType::End {
                continue;
            }
            if out_degree.get(node.key.as_str()).copied().unwrap_or(0) == 0 {
                issues.push(
                    Issue::new(
                        "R5",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "节点 {}（{}）没有出边（除结束节点外，每个节点都必须继续往下走）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R6：APPROVAL / COUNTERSIGN / CC 出度必须 = 1（架构 D7） ----------
        for node in &self.nodes {
            if !node.node_type.is_approval_class() {
                continue;
            }
            let outdegree = out_degree.get(node.key.as_str()).copied().unwrap_or(0);
            if outdegree != 1 {
                issues.push(
                    Issue::new(
                        "R6",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "审批节点 {}（{}）有 {outdegree} 条出边（审批节点只能有一条出边，分支请用条件网关）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R7：CONDITION / PARALLEL 出度 ≥ 2 ----------
        for node in &self.nodes {
            if !node.node_type.is_gateway() {
                continue;
            }
            let outdegree = out_degree.get(node.key.as_str()).copied().unwrap_or(0);
            if outdegree < 2 {
                issues.push(
                    Issue::new(
                        "R7",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "{}节点 {}（{}）只有 {outdegree} 条出边（条件 / 并行节点至少要有 2 条出边，否则请改用审批节点或删掉它）",
                            if node.node_type == NodeType::Condition {
                                "条件"
                            } else {
                                "并行"
                            },
                            node.key,
                            node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R8：禁止混流汇聚（架构 V8 / D6） ----------
        issues.extend(self.check_join_mode(&in_degree));

        // ---------- R9：approve_ratio ∈ (0,1] + 签名模式必填（架构 V10 / D3 + P3-06） ----------
        //
        // P3-06 口径（决策 D1/D2）：
        // - `COUNTERSIGN`：必须显式选模式（历史规则不动）；
        // - `APPROVAL`：**允许不选**（`NULL` 等价「或签」），但一旦选了 `RATIO` 就必须带
        //   (0,1] 的比例 —— 只给会签判比例会漏掉「审批节点选按比例但没填阈值」这一档，
        //   那类图发出去以后 `judge` 会拿 `ratio.unwrap_or(1.0)` 当全员签用，用户看到的是
        //   「按比例」却不按比例，属于静默错配。
        for node in &self.nodes {
            let mode = match node.node_type {
                NodeType::Countersign => match node.approval_mode {
                    Some(mode) => mode,
                    None => {
                        issues.push(
                            Issue::new(
                                "R9",
                                error_code::FLOW_NODE_ASSIGNEE_MISSING,
                                format!(
                                    "会签节点 {}（{}）未设置签名方式（或签 / 全员签 / 按比例，必须选一个）",
                                    node.key, node.name
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                        continue;
                    }
                },
                NodeType::Approval => match node.approval_mode {
                    Some(mode) => mode,
                    // 不选 = 或签（`ApprovalMode::effective`），发布期不报错
                    None => continue,
                },
                _ => continue,
            };
            if mode == ApprovalMode::Ratio {
                let kind = if node.node_type == NodeType::Countersign {
                    "会签节点"
                } else {
                    "审批节点"
                };
                match node.approve_ratio {
                    None => issues.push(
                        Issue::new(
                            "R9",
                            error_code::FLOW_NODE_ASSIGNEE_MISSING,
                            format!(
                                "{kind} {}（{}）选择了「按比例」但没有填通过比例",
                                node.key, node.name
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    ),
                    Some(ratio) if ratio <= 0.0 || ratio > 1.0 => issues.push(
                        Issue::new(
                            "R9",
                            error_code::FLOW_NODE_ASSIGNEE_MISSING,
                            format!(
                                "{kind} {} 的通过比例必须在 (0,1] 之间（当前 {ratio}）",
                                node.key
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    ),
                    Some(_) => {}
                }
            }
        }

        // ---------- R10：条件 DSL 合法（架构 V11 / D5） ----------
        for edge in &self.edges {
            let Some(condition) = edge.condition.as_ref() else {
                continue;
            };
            if let Err(reason) = parse_condition(condition) {
                issues.push(
                    Issue::new(
                        "R10",
                        error_code::FLOW_CONDITION_INVALID,
                        format!(
                            "边 {}（{} → {}）的条件表达式非法：{reason}",
                            edge.key, edge.from, edge.to
                        ),
                    )
                    .with_nodes(vec![edge.from.clone(), edge.to.clone()]),
                );
            }
        }

        // ---------- R11：所有节点从 START 可达（架构 V4） ----------
        if starts.len() == 1 {
            let reachable = self.reach_from(&starts);
            for node in &self.nodes {
                if !reachable.contains(&node.key) {
                    issues.push(
                        Issue::new(
                            "R11",
                            error_code::FLOW_GRAPH_STRUCTURE,
                            format!(
                                "节点 {}（{}）从开始节点走不到（悬空节点，请连上或删掉）",
                                node.key, node.name
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
            }
        }

        // ---------- R12：所有节点都能到达某个 END（架构 V2 的另一半） ----------
        if !ends.is_empty() {
            let can_reach_end = self.reach_to(&ends);
            for node in &self.nodes {
                if !can_reach_end.contains(&node.key) {
                    issues.push(
                        Issue::new(
                            "R12",
                            error_code::FLOW_GRAPH_STRUCTURE,
                            format!(
                                "节点 {}（{}）走不到结束节点（分支没有汇合，流程会在这里走丢）",
                                node.key, node.name
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
            }
        }

        // ---------- R13（架构 V7）：CONDITION 出边恰一条 default，其余必须带条件 ----------
        for node in &self.nodes {
            if node.node_type != NodeType::Condition {
                continue;
            }
            let outgoing: Vec<&EdgeSpec> = self
                .edges
                .iter()
                .filter(|edge| edge.from == node.key)
                .collect();
            let defaults = outgoing.iter().filter(|edge| edge.is_default).count();
            if defaults != 1 {
                issues.push(
                    Issue::new(
                        "R13",
                        error_code::FLOW_GRAPH_DEFAULT_EDGE,
                        format!(
                            "条件节点 {}（{}）有 {defaults} 条兜底分支（必须恰有 1 条兜底分支，否则某些取值下流程无路可走）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
            for edge in &outgoing {
                if !edge.is_default && edge.condition.is_none() {
                    issues.push(
                        Issue::new(
                            "R13",
                            error_code::FLOW_GRAPH_DEFAULT_EDGE,
                            format!(
                                "条件节点 {} 的出边 {}（{} → {}）既不是兜底分支、也没有条件（请补条件或设为兜底）",
                                node.key, edge.key, edge.from, edge.to
                            ),
                        )
                        .with_nodes(vec![node.key.clone(), edge.to.clone()]),
                    );
                }
                if edge.is_default && edge.condition.is_some() {
                    issues.push(
                        Issue::new(
                            "R13",
                            error_code::FLOW_GRAPH_DEFAULT_EDGE,
                            format!(
                                "条件节点 {} 的兜底分支 {} 不允许带条件（带了条件的兜底不是兜底）",
                                node.key, edge.key
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
            }
        }

        // ---------- R14（架构 V9）：审批类节点必须配置审批人 ----------
        issues.extend(self.check_assignees());

        // ---------- R15（架构 V12）：规模上限 ----------
        if self.nodes.len() > MAX_NODES {
            issues.push(Issue::new(
                "R15",
                error_code::FLOW_GRAPH_TOO_LARGE,
                format!(
                    "流程节点数 {} 超过上限 {MAX_NODES}（请拆成多个流程）",
                    self.nodes.len()
                ),
            ));
        }
        if self.edges.len() > MAX_EDGES {
            issues.push(Issue::new(
                "R15",
                error_code::FLOW_GRAPH_TOO_LARGE,
                format!(
                    "流程连线数 {} 超过上限 {MAX_EDGES}（请拆成多个流程）",
                    self.edges.len()
                ),
            ));
        }

        // ---------- R16（批次 D · EXPRESSION DSL §8.5）：表达式审批人必须可解析 ----------
        issues.extend(self.expression_issues());

        // ---------- R17（P-FLOW-1A / P3-08）：TASK_NODE / NOTIFY_NODE / SUBPROCESS 出度必须 = 1 ----------
        for node in &self.nodes {
            if !matches!(
                node.node_type,
                NodeType::Task | NodeType::Notify | NodeType::Subprocess
            ) {
                continue;
            }
            let outdegree = out_degree.get(node.key.as_str()).copied().unwrap_or(0);
            if outdegree != 1 {
                issues.push(
                    Issue::new(
                        "R17",
                        error_code::FLOW_GRAPH_DEGREE,
                        format!(
                            "{}节点 {}（{}）有 {outdegree} 条出边（只能有一条出边：任务完成 / 通知发完 / 子流程结算之后「往哪走」必须唯一；分支请用条件网关）",
                            match node.node_type {
                                NodeType::Task => "任务",
                                NodeType::Notify => "通知",
                                _ => "子流程",
                            },
                            node.key,
                            node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R18（设计 §2.7 / §2.3）：TASK_NODE 的 config.task ----------
        // 三件事：① 配置归属（task 只许出现在 TASK_NODE 上）② 形状 + 交叉约束（65034）
        // ③ need_review × on_complete 矩阵（65035）。
        for node in &self.nodes {
            let has_task = node.config.get("task").is_some_and(|v| !v.is_null());
            if node.node_type != NodeType::Task {
                // 分区纪律 1：`task` 放在别的类型上是**保存期 400**，不是静默忽略 ——
                // 静默忽略会让用户以为配上了（设计 §2.1 第 2 条）。
                if has_task {
                    issues.push(
                        Issue::new(
                            "R18",
                            error_code::FLOW_NODE_CONFIG_MISMATCH,
                            format!(
                                "节点 {}（{}）的类型是 {}，不允许配置任务配置（config.task）；任务配置只能放在 TASK_NODE 上",
                                node.key,
                                node.name,
                                node.node_type.as_str()
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
                continue;
            }
            if !has_task {
                issues.push(
                    Issue::new(
                        "R18",
                        error_code::FLOW_NODE_CONFIG_MISMATCH,
                        format!(
                            "任务节点 {}（{}）缺少 config.task（至少要配 title_template 与 on_complete）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
                continue;
            }
            if node.config.get("notify").is_some_and(|v| !v.is_null()) {
                issues.push(
                    Issue::new(
                        "R18",
                        error_code::FLOW_NODE_CONFIG_MISMATCH,
                        format!(
                            "任务节点 {}（{}）同时配了 config.notify（一个节点不能既是任务又是通知：阻塞语义自相矛盾）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
            let config = match node_config::parse_task_config(&node.config["task"]) {
                Ok(config) => config,
                Err(error) => {
                    issues.push(
                        Issue::new(
                            "R18",
                            error.code,
                            format!("节点 {}（{}）：{}", node.key, node.name, error.msg),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                    continue;
                }
            };
            if let Err(error) = node_config::check_matrix(&config) {
                issues.push(
                    Issue::new(
                        "R18",
                        error.code,
                        format!("节点 {}（{}）：{}", node.key, node.name, error.msg),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R19（设计 §2.7 · P-FLOW-2 放开）：NOTIFY_NODE 的配置完整性 ----------
        //
        // 1A 时这条规则是**阶段闸**（直接拒绝 NOTIFY_NODE，因为引擎没有分支）。
        // P-FLOW-2 补上了引擎分支与 `engine::fire_notify_node`，闸门换成**真正的配置校验**：
        //
        // ① `config.notify` 必须存在且形状合法（`node_config::parse_notify_config`，`65034` / `65036`）；
        // ② `when=ON_TIMEOUT` 必须配 `timeout_hours`（否则这个节点永远等不到「超时」这个时刻
        //    —— 正是「发布得出去、跑起来静默不生效」）；
        // ③ 内联模板的占位符必须①语法正确②在白名单内（与 R21 同一套 `template_field_allowed`）；
        // ④ **不**限制「NOTIFY_NODE 不能是第一个节点」：`START → NOTIFY(通知发起人) → APPROVAL`
        //    是最常见的用法（P-FLOW-2 验收用例 ① 就是这张图）。卡面把这条留成「按设计定」，
        //    设计的硬约束里没有它，所以不放这条限制。
        //
        // 出度 = 1 由 R17 管（同一条属性不两处报错）；模板存在性（DB 查 `sys_notice_template`）
        // 在发布 handler 里由 `notify_node::template_issues` 补（纯函数拿不到 DB）。
        for node in &self.nodes {
            if node.node_type != NodeType::Notify {
                continue;
            }
            let Some(raw) = node.config.get("notify") else {
                issues.push(
                    Issue::new(
                        "R19",
                        error_code::FLOW_NODE_CONFIG_MISMATCH,
                        format!(
                            "通知节点 {}（{}）缺少 config.notify（至少要配 when / targets 与模板二选一）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
                continue;
            };
            let config = match node_config::parse_notify_config(raw) {
                Ok(config) => config,
                Err(error) => {
                    issues.push(
                        Issue::new(
                            "R19",
                            error.code,
                            format!("节点 {}（{}）：{}", node.key, node.name, error.msg),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                    continue;
                }
            };

            // ② ON_TIMEOUT 必须有停留时长
            if config.when.is_deferred() && node.timeout_hours.is_none_or(|hours| hours <= 0) {
                issues.push(
                    Issue::new(
                        "R19",
                        error_code::FLOW_TIMEOUT_CONFIG_INVALID,
                        format!(
                            "通知节点 {}（{}）的时机是「超时后发送」，必须同时配置超时小时（timeout_hours > 0），\
                             否则这个节点永远等不到发送时刻",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }

            // ③ 内联模板占位符：语法 + 白名单
            for (label, template) in [
                ("inline.title", config.template.templates().first().copied()),
                (
                    "inline.content",
                    config.template.templates().get(1).copied(),
                ),
            ] {
                let Some(template) = template else {
                    continue;
                };
                let placeholders = match render::collect_placeholders_paths(template) {
                    Ok(names) => names,
                    Err(error) => {
                        issues.push(
                            Issue::new(
                                "R19",
                                error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
                                format!(
                                    "通知节点 {}（{}）的 {label} 模板语法错误：{}",
                                    node.key, node.name, error.msg
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                        continue;
                    }
                };
                for name in placeholders {
                    if !template_field_allowed(&name) {
                        issues.push(
                            Issue::new(
                                "R19",
                                error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
                                format!(
                                    "通知节点 {}（{}）的 {label} 用了 {{{{{name}}}}}，不在白名单内\
                                     （允许 form.* / instance.* / initiator.* / node.* / prev.*）",
                                    node.key, node.name
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                    }
                }
            }
        }

        // ---------- R20（设计 §2.4 / §2.7）：TASK_NODE 的指派与超时纪律 ----------
        // ① 指派来源（缺失 / 可枚举但空 / FORM_VAR 无变量名）走 `check_assignees` 的
        //    共用判定（规则编号由节点类型决定，不重复报错）。
        // ② `empty_assignee=SKIP` 对任务节点是**静默丢活**，必须额外确认。
        // ③ `timeout_action` 只允许 REMIND / ESCALATE（自动通过 / 自动驳回会把「没干」写成「干了」）。
        for node in &self.nodes {
            if node.node_type != NodeType::Task {
                continue;
            }
            let config = node_config::parse_task_config(&node.config["task"]).ok();
            if node.empty_assignee.eq_ignore_ascii_case("SKIP")
                && !config
                    .as_ref()
                    .is_some_and(|cfg| cfg.allow_skip_when_no_assignee)
            {
                issues.push(
                    Issue::new(
                        "R20",
                        error_code::FLOW_NO_ASSIGNEE,
                        format!(
                            "任务节点 {}（{}）的空负责人策略是「跳过」，那意味着派不出人时这个活直接不干了（静默丢活）。\
                             要保留请同时打开 config.task.allow_skip_when_no_assignee=true，否则请改成 ERROR 或 FALLBACK_ADMIN",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
            let action = node.timeout_action.trim().to_ascii_uppercase();
            if !matches!(action.as_str(), "REMIND" | "ESCALATE") {
                issues.push(
                    Issue::new(
                        "R20",
                        error_code::FLOW_TIMEOUT_CONFIG_INVALID,
                        format!(
                            "任务节点 {}（{}）的超时动作是 {action}（只允许 REMIND 提醒 / ESCALATE 升级：活没干完不能因为超时就算干完或被驳回）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }

        // ---------- R21（设计 §2.5.1 / §2.7）：模板变量路径白名单 ----------
        // 发布期静态扫描（不是运行期 422）：占位符写错是最高频的配置错误。
        issues.extend(self.template_issues(biz_type));

        // ---------- R22（P3-03 / ARCH-8 场景 C）：节点兜底人配置纪律 ----------
        issues.extend(self.fallback_issues());

        // ---------- R27（P3-08 / ARCH-5 §4.14）：SUBPROCESS 节点配置纪律 ----------
        issues.extend(self.subprocess_issues());

        issues
    }

    /// R27（纯函数部分）：`SUBPROCESS` 节点的静态配置纪律（P3-08 / ARCH-5 §4.14）。
    ///
    /// # 这里能查什么、不能查什么
    ///
    /// `validate_with` 没有 `SqlExecutor`（60 多条存量单测都靠这一点），所以：
    ///
    /// | 判据 | 在哪 | 错误码 |
    /// |---|---|---|
    /// | `subprocess_def_id` 必填 | 本函数（列上的值随快照进内存） | `65045` ⓐ |
    /// | `config.sub` 形状 / 枚举取值 | 本函数（[`node_config::parse_sub_config`]） | `65045` ⓑ |
    /// | `config.sub` 只许出现在 SUBPROCESS 上（分区纪律） | 本函数 | `65045` ⓒ |
    /// | 出度 = 1 | R17（`is_single_exit` 已含 Subprocess） | `65010` |
    /// | 定义存在 / 已发布 / 不自递归 / 深度 ≤ 3 / param_mapping 键存在 | [`super::subprocess::validate_publish`]（要查库） | `65045` / `65046` |
    ///
    /// 自递归与深度**不能**在这里判：`subprocess_def_id` 指向的是另一张图，
    /// 它的子流程关系只有查库才知道（`biz_flow_node` 的另一行）。
    pub fn subprocess_issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        for node in &self.nodes {
            let has_sub = node.config.get("sub").is_some_and(|value| !value.is_null());
            if node.node_type != NodeType::Subprocess {
                // 分区纪律（与 R18 的 task / R19 的 notify 同款）：静默忽略比报错更坏 ——
                // 用户以为配上了，实际引擎根本不读。
                if has_sub {
                    issues.push(
                        Issue::new(
                            "R27",
                            error_code::FLOW_SUBPROCESS_DEF_INVALID,
                            format!(
                                "节点 {}（{}）的类型是 {}，不允许配置子流程配置（config.sub）；子流程配置只能放在 SUBPROCESS 上",
                                node.key,
                                node.name,
                                node.node_type.as_str()
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
                if node.subprocess_def_id.is_some() {
                    issues.push(
                        Issue::new(
                            "R27",
                            error_code::FLOW_SUBPROCESS_DEF_INVALID,
                            format!(
                                "节点 {}（{}）的类型是 {}，却配了「子流程定义」；只有 SUBPROCESS 节点能指向子流程",
                                node.key,
                                node.name,
                                node.node_type.as_str()
                            ),
                        )
                        .with_nodes(vec![node.key.clone()]),
                    );
                }
                continue;
            }

            // ⓐ 必须指向一个子流程定义
            if node.subprocess_def_id.is_none() {
                issues.push(
                    Issue::new(
                        "R27",
                        error_code::FLOW_SUBPROCESS_DEF_INVALID,
                        format!(
                            "子流程节点 {}（{}）没有选择要执行的子流程（subprocess_def_id 为空）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }

            // ⓑ 形状 / 枚举（缺 config.sub 不报错：五个键全有默认值，空对象是合法的最简配置）
            if has_sub && let Err(error) = node_config::parse_sub_config(&node.config["sub"]) {
                issues.push(
                    Issue::new(
                        "R27",
                        error.code,
                        format!(
                            "子流程节点 {}（{}）：{}",
                            node.key, node.name, error.message
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }
        issues
    }

    /// R22：节点兜底人配置纪律（P3-03）。
    ///
    /// 三条判据（**发布期**静态检查，全部 `65042`）：
    ///
    /// 1. `fallback_user_id` 与 `fallback_role_id` 同时配置 → 「到底听谁」的歧义。
    ///    DB 有 `ck_biz_flow_node_fallback_exclusive` 挡着，这里提前一步给出人读文案
    ///    （否则用户看到的是 `new row violates check constraint …` 这种库错误）；
    /// 2. 兜底人与节点 `assignee_type=USER` 的指定人完全重合 → 无意义配置：
    ///    主路就是他、兜底还是他，等于「没换人」（与卡面「不得等于同一人」一致）；
    /// 3. 兜底配置放在**没有审批语义**的节点上（START / END / 网关 / 任务 / 通知）→ 静默
    ///    忽略比报错更坏：用户以为配上了，实际上解析审批人的代码根本不会读这两列。
    ///
    /// # 为什么不校验「角色 / 用户是否存在」
    ///
    /// `validate_with` 是**纯函数**（没有 `SqlExecutor`，60 多条存量单测都直接调它）。
    /// 存在性校验交给运行期 `assignee::apply_fallback` 的 `65042`（用户已删 / 已停用 /
    /// 角色已删 / 角色 0 人），那里能一次说清「哪一列、为什么」。
    /// 这里只做**格式**校验（UUID 解析在 `dto` 保存期已完成，快照里已是合法 UUID 字符串）。
    pub fn fallback_issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        for node in &self.nodes {
            let user = node.fallback_user_id.as_ref();
            let role = node.fallback_role_id.as_ref();

            // ① 二选一
            if user.is_some() && role.is_some() {
                issues.push(
                    Issue::new(
                        "R22",
                        error_code::FLOW_FALLBACK_INVALID,
                        format!(
                            "节点 {}（{}）同时配置了「兜底审批人」与「兜底角色」——只能二选一\
                             （两个都配，运行时到底听谁的？要多人兜底就配角色）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
                // 后面两条判据都基于「配置本身有意义」，这里已经报了，不再叠加
                continue;
            }

            // ② 兜底人 == 指定审批人（USER 来源）
            if let Some(fallback_user) = user
                && node.assignee_type == Some(AssigneeKind::User)
                && node.assignees.iter().any(|assignee| {
                    assignee.kind.eq_ignore_ascii_case("USER")
                        && assignee.ref_id == fallback_user.to_string()
                })
            {
                issues.push(
                    Issue::new(
                        "R22",
                        error_code::FLOW_FALLBACK_INVALID,
                        format!(
                            "节点 {}（{}）的「兜底审批人」与「指定审批人」是同一个人：\
                             主路就是他、兜底还是他，等于没有兜底（请换一个非本人的人，或改配兜底角色）",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }

            // ③ 配在无审批语义的节点上
            if (user.is_some() || role.is_some()) && !node.node_type.is_approval_class() {
                issues.push(
                    Issue::new(
                        "R22",
                        error_code::FLOW_FALLBACK_INVALID,
                        format!(
                            "节点 {}（{}）的类型是 {}，不允许配置兜底审批人（只有审批 / 会签 / 抄送节点\
                             解析审批人，配置在其它类型上不会被读取）",
                            node.key,
                            node.name,
                            node.node_type.as_str()
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }
        issues
    }

    /// R21：`config.task` 里所有模板的变量路径必须在白名单内。
    ///
    /// # 三条判据（设计 §2.5.1）
    ///
    /// 1. 路径必须 `template_field_allowed()`（`FIXED_FIELDS ∪ TEMPLATE_ONLY_FIELDS ∪ form.*`）
    ///    → `65037`；
    /// 2. `task.*` 只在 `biz_type = 'TASK_ACCEPT'` 的流程里可用（`EvalContext.task` 只在那个场景有值）
    ///    → `65037`；
    /// 3. `due.mode=FIELD` 的字段必须是 `form.*`（期限只能从表单取）→ `65034`。
    ///
    /// 模板语法错（占位符未闭合 / 变量名非法 / 超过一层点号）→ `65036`，与预演接口同码。
    fn template_issues(&self, biz_type: Option<&str>) -> Vec<Issue> {
        let mut issues = Vec::new();
        let task_accept = biz_type
            .map(|raw| raw.trim().eq_ignore_ascii_case("TASK_ACCEPT"))
            .unwrap_or(false);
        for node in &self.nodes {
            if node.node_type != NodeType::Task {
                continue;
            }
            let Ok(config) = node_config::parse_task_config(&node.config["task"]) else {
                // 形状已经不合法（R18 已报过一条），这里不重复报「模板变量」问题。
                continue;
            };
            for (label, template) in [
                ("title_template", Some(config.title_template.as_str())),
                (
                    "description_template",
                    config.description_template.as_deref(),
                ),
            ] {
                let Some(template) = template else {
                    continue;
                };
                let placeholders = match render::collect_placeholders_paths(template) {
                    Ok(names) => names,
                    Err(error) => {
                        // 语法错（未闭合 / 变量名非法 / 超过一层点号）统一抬成 `65036`：
                        // 60024 是「通知模板管理」那套码，流程发布校验不该混进另一个域
                        // （设计 §3.3 的预演错误表也把「模板语法错」记在 65036 上）。
                        issues.push(
                            Issue::new(
                                "R21",
                                error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
                                format!(
                                    "节点 {}（{}）的 {label} 模板语法错误：{}",
                                    node.key, node.name, error.msg
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                        continue;
                    }
                };
                for name in placeholders {
                    if !template_field_allowed(&name) {
                        issues.push(
                            Issue::new(
                                "R21",
                                error_code::FLOW_TEMPLATE_VAR_INVALID,
                                format!(
                                    "节点 {}（{}）的 {label} 用了 {{% raw %}}{{{name}}}{{% endraw %}}，不在白名单内\
                                     （允许 form.* / instance.* / initiator.* / node.* / prev.* / task.*）",
                                    node.key, node.name
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                        continue;
                    }
                    if name.starts_with("task.") && !task_accept {
                        issues.push(
                            Issue::new(
                                "R21",
                                error_code::FLOW_TEMPLATE_VAR_INVALID,
                                format!(
                                    "节点 {}（{}）的 {label} 用了 {{{{task.*}}}}，但本流程的业务类型是 {}（task.* 只在 TASK_ACCEPT 流程里有值，运行期会渲染成「缺变量」）",
                                    node.key,
                                    node.name,
                                    biz_type.unwrap_or("（未知）")
                                ),
                            )
                            .with_nodes(vec![node.key.clone()]),
                        );
                    }
                }
            }
            if let Some(due) = config.due.as_ref()
                && due.mode == node_config::DueMode::Field
                && !due
                    .field
                    .as_deref()
                    .map(|field| field.starts_with("form."))
                    .unwrap_or(false)
            {
                issues.push(
                    Issue::new(
                        "R21",
                        error_code::FLOW_NODE_CONFIG_MISMATCH,
                        format!(
                            "节点 {}（{}）的期限取自字段 {}，但只有 form.* 字段能用（期限必须来自表单）",
                            node.key,
                            node.name,
                            due.field.as_deref().unwrap_or("（空）")
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }
        issues
    }

    /// R16：`assignee_type = EXPRESSION` 的节点，`config.expression` 必须存在且能解析。
    ///
    /// **两个调用方共用**：发布校验（[`FlowGraph::validate`] 的 R16）与保存期
    /// （`dto::validate_expression_nodes`）—— 保存期拦住 99% 的坏配置，发布期做双保险
    /// （防旧快照 / 有人直接改库）。两处共用一个实现，文案与判据不可能漂移。
    ///
    /// # 判定口径（EXPRESSION-DSL 规范 §3.6 第 8 行 / §5.5 / §8.5）
    ///
    /// | 情形 | 码 | 为什么 |
    /// |---|---|---|
    /// | `config.expression` 缺失 / 结构非法 / 上限超了 | `65016` | 保存期就该挡住，发布期双保险（防旧快照 / 直接改库） |
    /// | 表达式引用了 `PREV_APPROVER` | **`65033`** | D-E8：本期不开放这能力，必须在**发布期**拒绝，不能「存进去、跑起来才 501」 |
    ///
    /// 为什么必须**发布期**拒绝而不是运行期 501（D-E8 原话）：
    /// 落 501 意味着「能存进去、跑起来才炸」—— 那正是本期要消灭的形态；发布期拒绝是「第一次保存时就告诉你」。
    /// 注意保存期（`dto::validate_expression_nodes`）只做结构校验、不专门拒 `PREV_APPROVER`，
    /// 所以这里是这条红线唯一的拦截点（草稿允许存、发布必须拦）。
    pub fn expression_issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        for node in &self.nodes {
            if node.assignee_type != Some(AssigneeKind::Expression) {
                continue;
            }
            // ① 保留词先判（65033 比「结构非法」更能说明问题：用户是想要一个本期没有的能力）
            if let Some(value) = node.config.get("expression")
                && expression_uses_prev_approver(value)
            {
                issues.push(
                    Issue::new(
                        "R16",
                        error_code::FLOW_EXPRESSION_INVALID,
                        format!(
                            "节点 {}（{}）的审批人表达式用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
                continue;
            }
            // ② 结构 / 上限 / 字段白名单 / 规则 id 唯一 —— 与保存期、运行期**同一个**解析器
            if let Err(reason) = expression::parse_config(&node.config) {
                issues.push(
                    Issue::new(
                        "R16",
                        error_code::FLOW_CONDITION_INVALID,
                        format!(
                            "节点 {}（{}）的审批人表达式不合法：{reason}",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(vec![node.key.clone()]),
                );
            }
        }
        issues
    }

    /// R8：禁止混流汇聚（架构 §3.5.3 / D6）。
    ///
    /// 口径（逐条对应架构表格）：
    ///
    /// | 入边来源 | 允许的 join_mode | 违反后果 |
    /// |----------|------------------|----------|
    /// | 全部来自同一个 `CONDITION` | 只能 `OR` | 用 `AND` → 有一支永不激活 → 永久卡死 |
    /// | 全部来自同一个 `PARALLEL` | 只能 `AND` | 用 `OR` → 并行任务提前放行 |
    /// | 来自不同网关 / 找不到网关 | 禁止 | 语义无法定义 |
    ///
    /// **END 不参与**：结束节点不做汇聚门控，两个分支各自连到 END 是合法建模
    /// （否则「条件 A 走 end1、条件 B 走 end2」会被误报）。
    ///
    /// # END 的运行期口径（P3-09 B2b 修复后两边逐字一致）
    ///
    /// 作者**不需要**给 END 填 `join_mode`，填了也不参与运行期判定：
    /// `engine::Run::step` 对 END 不再按「入边是否都到达」门控，而是判「本轮上游还有没有
    /// 在跑的节点」（`engine::Run::end_waits_upstream`）。条件分支只走一支 → 到达即结束；
    /// 并行分支两支都在跑 → 等它们跑完再结束。
    ///
    /// 修复前：校验侧跳过 END（本节口径），运行期却按 `join_mode.unwrap_or(AND)` 等齐**入边** ⇒
    /// 条件多分支汇到同一个 END 时另几支永远不来，END 停在 `PENDING`、实例永久 `PENDING`，
    /// 且没有任何报错（P3-09 场景 B2b 实测）。详见 `docs/工作流END汇聚语义修复.md`。
    fn check_join_mode(&self, in_degree: &HashMap<&str, usize>) -> Vec<Issue> {
        let mut issues = Vec::new();
        let in_map = self.in_edges();
        for node in &self.nodes {
            if node.node_type == NodeType::End {
                continue;
            }
            if in_degree.get(node.key.as_str()).copied().unwrap_or(0) < 2 {
                continue;
            }
            let incoming = in_map
                .get(node.key.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut sources: Vec<(String, NodeType, String)> = Vec::new();
            let mut unresolved: Vec<String> = Vec::new();
            for edge in incoming {
                match self.nearest_upstream_fork(&edge.from) {
                    Some((fork_key, fork_type)) => {
                        sources.push((fork_key, fork_type, edge.from.clone()))
                    }
                    None => unresolved.push(edge.from.clone()),
                }
            }

            if !unresolved.is_empty() {
                issues.push(
                    Issue::new(
                        "R8",
                        error_code::FLOW_GRAPH_JOIN_MODE,
                        format!(
                            "汇聚节点 {}（{}）的入边 {}{} 上游没有分支网关（汇聚必须接在条件 / 并行节点之后）",
                            node.key,
                            node.name,
                            unresolved.join("、"),
                            if unresolved.len() > 1 { " 等" } else { "" }
                        ),
                    )
                    .with_nodes(
                        std::iter::once(node.key.clone())
                            .chain(unresolved.iter().cloned())
                            .collect(),
                    ),
                );
                continue;
            }

            let fork_keys: HashSet<&str> = sources.iter().map(|(key, _, _)| key.as_str()).collect();
            if fork_keys.len() > 1 {
                let detail = sources
                    .iter()
                    .map(|(fork_key, _, from)| format!("{from} ← {fork_key}"))
                    .collect::<Vec<_>>()
                    .join("，");
                issues.push(
                    Issue::new(
                        "R8",
                        error_code::FLOW_GRAPH_JOIN_MODE,
                        format!(
                            "汇聚节点 {}（{}）的入边来自不同分支网关（{detail}）—— 禁止混流汇聚，请让同一网关的分支汇到同一个节点",
                            node.key, node.name
                        ),
                    )
                    .with_nodes(std::iter::once(node.key.clone()).chain(fork_keys.iter().map(|k| k.to_string())).collect()),
                );
                continue;
            }

            let (fork_key, fork_type, _) = &sources[0];
            let expected = match fork_type {
                NodeType::Condition => GatewayMode::Or,
                _ => GatewayMode::And,
            };
            if node.join_mode != expected {
                let kind_text = if *fork_type == NodeType::Condition {
                    "条件分支（XOR）"
                } else {
                    "并行网关（AND）"
                };
                let consequence = if *fork_type == NodeType::Condition {
                    "用 AND 汇聚会有一条分支永远不激活 → 审批单永久卡死"
                } else {
                    "用 OR 汇聚会让并行任务提前放行（另一半还没审）"
                };
                issues.push(
                    Issue::new(
                        "R8",
                        error_code::FLOW_GRAPH_JOIN_MODE,
                        format!(
                            "汇聚节点 {}（{}）的入边来自{fork_key}的{kind_text}，必须用 {} 汇聚（当前 {}）—— {consequence}",
                            node.key,
                            node.name,
                            expected.as_str(),
                            node.join_mode.as_str()
                        ),
                    )
                    .with_nodes(vec![node.key.clone(), fork_key.clone()]),
                );
            }
        }
        issues
    }

    /// R14：审批类节点的审批人配置（架构 §3.7 V9）。
    ///
    /// # 判定口径（E2 修正）
    ///
    /// 审批人有两个正交字段：来源类型 `assignee_type` 与具体目标 `assignees`。
    /// 「配了审批人」= **两者任一有内容**：
    ///
    /// - 只给 `assignees`（前端画布常见形态）：来源类型由落行目标反推
    ///   （`dto::GraphSaveReq::into_graph` 已回填；历史行 / 直接写库的行可能是空），
    ///   这里再反推一次兜底 —— 否则「明明选了 3 个审批人」仍被判 65015；
    /// - 只给 `assignee_type`（`INITIATOR` / `ADMIN` 等不可枚举来源）：不加附加要求；
    /// - 两个都不给：65015。
    fn check_assignees(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        for node in &self.nodes {
            if !node.node_type.needs_assignee() {
                continue;
            }
            // 同一个判定、两种用词：审批节点叫「审批人」（R14 / 65015），
            // 任务节点叫「负责人」（R20 / 65015）。**不重复报错** —— 一个节点只落一条。
            let (rule, noun) = if node.node_type == NodeType::Task {
                ("R20", "负责人")
            } else {
                ("R14", "审批人")
            };
            issues.extend(assignee_source_issues(node, rule, noun));
        }
        issues
    }
}

/// 「这个节点配了指派来源吗」的共用判定（R14 / R20 的唯一实现）。
///
/// 抽成自由函数而不是两处各写一遍：审批人与负责人的判据必须逐字相同，
/// 否则「角色里 0 人」这类边界会在两张卡之间漂移（一个拦、一个不拦）。
fn assignee_source_issues(node: &NodeSpec, rule: &'static str, noun: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(kind) = node
        .assignee_type
        .or_else(|| infer_assignee_kind(&node.assignees))
    else {
        issues.push(
            Issue::new(
                rule,
                error_code::FLOW_NODE_ASSIGNEE_MISSING,
                format!(
                    "节点 {}（{}）未配置{noun}（请选择{noun}来源：指定用户 / 角色 / 发起人 / 主管 …）",
                    node.key, node.name
                ),
            )
            .with_nodes(vec![node.key.clone()]),
        );
        return issues;
    };
    if kind.is_enumerable() && node.assignees.is_empty() {
        issues.push(
            Issue::new(
                rule,
                error_code::FLOW_NODE_ASSIGNEE_MISSING,
                format!(
                    "节点 {}（{}）的{noun}来源是 {}，但一个{noun}都没选",
                    node.key,
                    node.name,
                    kind.as_str()
                ),
            )
            .with_nodes(vec![node.key.clone()]),
        );
    }
    if kind == AssigneeKind::FormVar
        && node
            .form_var_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .is_none()
    {
        issues.push(
            Issue::new(
                rule,
                error_code::FLOW_NODE_ASSIGNEE_MISSING,
                format!(
                    "节点 {}（{}）的{noun}来源是「表单变量」，但没有填变量名",
                    node.key, node.name
                ),
            )
            .with_nodes(vec![node.key.clone()]),
        );
    }
    issues
}

/// 表达式里是否引用了保留词 `PREV_APPROVER`（D-E8：发布期必须拒绝）。
///
/// # 为什么按 JSON 扫而不是看解析器的错误文案
///
/// `expression::parse_expression` 对保留词与其他结构错误都只返回一个中文 `String`；
/// 若用「错误文案里有没有这个词」分流，等于让错误文案变成控制流（改一个字就静默退化成 65016）。
/// 这里直接在原始 JSON 上扫 `then` / `otherwise` 的 `type` 字段：判据只依赖**协议**，与文案无关。
///
/// 扫描范围只认 `rules[].then[]` 与 `otherwise[]`（协议里 `type` 只出现在这两处），
/// 因此 `when` 子树里的同名字段不会被误判。
pub fn expression_uses_prev_approver(value: &Value) -> bool {
    fn is_prev(value: &Value) -> bool {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(|raw| {
                raw.trim()
                    .eq_ignore_ascii_case(expression::RESERVED_PREV_APPROVER)
            })
            .unwrap_or(false)
    }

    let targets_hit = |targets: &Value| {
        targets
            .as_array()
            .map(|items| items.iter().any(is_prev))
            .unwrap_or(false)
    };

    if value
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .any(|rule| rule.get("then").map(targets_hit).unwrap_or(false))
        })
        .unwrap_or(false)
    {
        return true;
    }

    value.get("otherwise").map(targets_hit).unwrap_or(false)
}

/// 由落行审批人（`assignees`）反推来源类型（E2）。
///
/// R14 需要「只给具体目标、没给来源类型」也能通过校验。语义上 `assignees` 里每一条的
/// `kind` 就是它自己的来源类型，一个节点只允许一种来源（`dto::infer_assignee_type` 已挡混用），
/// 所以取第一条可解析的 `kind` 即为该节点的来源类型。
///
/// # 返回值
///
/// - `Some(kind)`：至少一条审批人且 `kind` 合法（全是合法可枚举来源）；
/// - `None`：`assignees` 为空，或 `kind` 一个都解析不出来 → 调用方按「未配置审批人」处理。
fn infer_assignee_kind(assignees: &[AssigneeRef]) -> Option<AssigneeKind> {
    assignees
        .iter()
        .find_map(|assignee| AssigneeKind::parse(&assignee.kind))
}

// ==================== 条件 DSL（架构 §3.4 / D5） ====================

/// 比较算子（架构 §3.4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareOp {
    /// 等于。
    Eq,
    /// 不等于。
    Ne,
    /// 大于。
    Gt,
    /// 大于等于。
    Gte,
    /// 小于。
    Lt,
    /// 小于等于。
    Lte,
    /// 在集合内。
    In,
    /// 不在集合内。
    Nin,
    /// 包含。
    Contains,
    /// 为空。
    IsNull,
    /// 非空。
    NotNull,
}

impl CompareOp {
    /// DSL 里的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            CompareOp::Eq => "eq",
            CompareOp::Ne => "ne",
            CompareOp::Gt => "gt",
            CompareOp::Gte => "gte",
            CompareOp::Lt => "lt",
            CompareOp::Lte => "lte",
            CompareOp::In => "in",
            CompareOp::Nin => "nin",
            CompareOp::Contains => "contains",
            CompareOp::IsNull => "is_null",
            CompareOp::NotNull => "not_null",
        }
    }

    /// 解析。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "eq" => Some(CompareOp::Eq),
            "ne" => Some(CompareOp::Ne),
            "gt" => Some(CompareOp::Gt),
            "gte" => Some(CompareOp::Gte),
            "lt" => Some(CompareOp::Lt),
            "lte" => Some(CompareOp::Lte),
            "in" => Some(CompareOp::In),
            "nin" => Some(CompareOp::Nin),
            "contains" => Some(CompareOp::Contains),
            "is_null" => Some(CompareOp::IsNull),
            "not_null" => Some(CompareOp::NotNull),
            _ => None,
        }
    }

    /// 是否不需要右值（`is_null` / `not_null`）。
    pub fn is_unary(self) -> bool {
        matches!(self, CompareOp::IsNull | CompareOp::NotNull)
    }
}

/// 条件表达式（强类型 enum：**没有「函数调用」这个面**，永久无代码执行，架构 D5）。
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// 逻辑与。
    And(Vec<Condition>),
    /// 逻辑或。
    Or(Vec<Condition>),
    /// 逻辑非（只有一个子节点）。
    Not(Box<Condition>),
    /// 比较。
    Cmp {
        /// 字段全名（如 `form.amount`）。
        field: String,
        /// 比较算子。
        cmp: CompareOp,
        /// 右值字面量。
        value: Value,
    },
}

/// 非 `form.*` 的固定字段白名单。
pub const FIXED_FIELDS: [&str; 10] = [
    "initiator.id",
    "initiator.name",
    "initiator.dept_id",
    "instance.biz_type",
    "instance.biz_no",
    "instance.title",
    "task.status",
    "task.priority",
    "task.estimate_hours",
    "task.title",
];

/// 字段白名单判定（`form.*` 允许任意合法标识符，其余必须逐字命中）。
pub fn field_allowed(field: &str) -> bool {
    let trimmed = field.trim();
    if FIXED_FIELDS.contains(&trimmed) {
        return true;
    }
    let Some(rest) = trimmed.strip_prefix("form.") else {
        return false;
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    !rest.is_empty()
        && rest.chars().count() <= 64
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// 只给**模板**用的字段根（设计 §2.5.1 的「两套白名单」）。
///
/// # 为什么条件 DSL 不认这 4 个
///
/// 条件的求值时机是「我该走哪条边」—— 此时当前节点还没结束（`node.name` 变成自我指涉），
/// 而 `prev.approver` 在第一个 START 之后根本不存在（`Missing` 在条件求值里按「不成立」
/// 处理 ⇒ 静默走错分支）。模板的求值时机是「节点已激活、要填一句话」，node / prev 都是确定值。
///
/// 一句话：**同一个变量命名空间，不同的可用子集，由求值时机决定**。
pub const TEMPLATE_ONLY_FIELDS: [&str; 4] = [
    "node.name",
    "node.key",
    "prev.approver_name",
    "prev.approver_id",
];

/// 模板变量路径白名单（R21 的判据）：`FIXED_FIELDS(10) ∪ TEMPLATE_ONLY_FIELDS(4) ∪ form.*`。
///
/// `task.*` 同属 `FIXED_FIELDS`，但它只在 `biz_type = 'TASK_ACCEPT'` 的流程里有值
/// （`EvalContext.task` 由 `engine::eval_context` 仅在该场景填充），所以「是否允许 `task.*`」
/// 由调用方（R21）按流程 `biz_type` 再收一次，本函数只管「路径拼写是否合法」。
pub fn template_field_allowed(field: &str) -> bool {
    field_allowed(field) || TEMPLATE_ONLY_FIELDS.contains(&field.trim())
}

/// 解析并校验条件表达式（架构 §3.4.1 的全部上限：深度 ≤ 8、节点 ≤ 64、args ≤ 16、字面量约束）。
///
/// 返回中文原因（直接拼进 `65016` 的文案）。
pub fn parse_condition(value: &Value) -> Result<Condition, String> {
    let mut nodes = 0usize;
    parse_condition_inner(value, 1, &mut nodes)
}

fn parse_condition_inner(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<Condition, String> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(format!(
            "条件嵌套层数超过 {MAX_CONDITION_DEPTH} 层（请把条件拆成几条边）"
        ));
    }
    *nodes += 1;
    if *nodes > MAX_CONDITION_NODES {
        return Err(format!(
            "条件表达式节点数超过 {MAX_CONDITION_NODES} 个（请把条件拆成几条边）"
        ));
    }
    let Some(object) = value.as_object() else {
        return Err("条件节点必须是 JSON 对象（如 {\"op\":\"cmp\",…}）".to_string());
    };
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "条件节点缺少 op 字段（and / or / not / cmp）".to_string())?;

    match op.as_str() {
        "and" | "or" => {
            let args = object
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{op} 节点必须有 args 数组"))?;
            if args.is_empty() {
                return Err(format!("{op} 的 args 不能为空"));
            }
            if args.len() > MAX_CONDITION_ARGS {
                return Err(format!("{op} 的 args 最多 {MAX_CONDITION_ARGS} 项"));
            }
            let parsed = args
                .iter()
                .map(|arg| parse_condition_inner(arg, depth + 1, nodes))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(if op == "and" {
                Condition::And(parsed)
            } else {
                Condition::Or(parsed)
            })
        }
        "not" => {
            let args = object
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| "not 节点必须有 args 数组（且只放一个条件）".to_string())?;
            if args.len() != 1 {
                return Err("not 的 args 必须恰好 1 项".to_string());
            }
            let inner = parse_condition_inner(&args[0], depth + 1, nodes)?;
            Ok(Condition::Not(Box::new(inner)))
        }
        "cmp" => {
            let field = object
                .get("field")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .ok_or_else(|| "cmp 节点缺少 field".to_string())?;
            if !field_allowed(field) {
                return Err(format!(
                    "字段 {field} 不在白名单内（允许 form.*、initiator.id/name/dept_id、instance.biz_type/biz_no/title、task.status/priority/estimate_hours）"
                ));
            }
            let cmp_raw = object
                .get("cmp")
                .and_then(Value::as_str)
                .ok_or_else(|| "cmp 节点缺少 cmp（比较符）".to_string())?;
            let cmp = CompareOp::parse(cmp_raw).ok_or_else(|| {
                format!("不支持的比较符 {cmp_raw}（允许 eq/ne/gt/gte/lt/lte/in/nin/contains/is_null/not_null）")
            })?;
            let value = if cmp.is_unary() {
                Value::Null
            } else {
                let literal = object
                    .get("value")
                    .ok_or_else(|| format!("cmp({}) 缺少 value", cmp.as_str()))?;
                check_literal(literal)?;
                literal.clone()
            };
            if (cmp == CompareOp::In || cmp == CompareOp::Nin) && !value.is_array() {
                return Err(format!("{} 的 value 必须是数组", cmp.as_str()));
            }
            Ok(Condition::Cmp {
                field: field.to_string(),
                cmp,
                value,
            })
        }
        other => Err(format!(
            "不支持的算子 {other}（只允许 and / or / not / cmp）"
        )),
    }
}

/// 字面量校验：只允许 string（≤200 字符）/ number / bool / null / array（≤50 项，元素为前三种）。
fn check_literal(value: &Value) -> Result<(), String> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(text) => {
            if text.chars().count() > MAX_LITERAL_CHARS {
                return Err(format!("字符串字面量超过 {MAX_LITERAL_CHARS} 个字符"));
            }
            Ok(())
        }
        Value::Array(items) => {
            if items.len() > MAX_LITERAL_ARRAY {
                return Err(format!("数组字面量最多 {MAX_LITERAL_ARRAY} 项"));
            }
            for item in items {
                match item {
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
                    _ => return Err("数组字面量只允许 string / number / bool / null".to_string()),
                }
            }
            Ok(())
        }
        Value::Object(_) => Err("字面量不允许对象（条件里没有「结构体」这个面）".to_string()),
    }
}

/// 求值错误（架构 §3.4.2：字段缺失是 `Missing`，**不是** `false`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// 字段不在白名单。
    UnknownField(String),
    /// 字段不存在（走 default 边并记 `condition_error`）。
    Missing(String),
    /// 类型不匹配。
    TypeMismatch(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UnknownField(field) => write!(formatter, "UnknownField: {field}"),
            EvalError::Missing(field) => write!(formatter, "Missing: {field}"),
            EvalError::TypeMismatch(detail) => write!(formatter, "TypeMismatch: {detail}"),
        }
    }
}

/// 求值上下文（发起时冻结的 `form_data` + 三类系统字段）。
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// `form.*`。
    pub form: Map<String, Value>,
    /// `initiator.*`。
    pub initiator: Map<String, Value>,
    /// `instance.*`。
    pub instance: Map<String, Value>,
    /// `task.*`（仅 `TASK_ACCEPT` 场景）。
    pub task: Map<String, Value>,
}

impl EvalContext {
    /// 取字段值：未命中 → [`EvalError::Missing`]。
    pub fn resolve(&self, field: &str) -> Result<&Value, EvalError> {
        if !field_allowed(field) {
            return Err(EvalError::UnknownField(field.to_string()));
        }
        let (root, leaf) = field
            .split_once('.')
            .ok_or_else(|| EvalError::UnknownField(field.to_string()))?;
        let map = match root {
            "form" => &self.form,
            "initiator" => &self.initiator,
            "instance" => &self.instance,
            "task" => &self.task,
            _ => return Err(EvalError::UnknownField(field.to_string())),
        };
        map.get(leaf)
            .ok_or_else(|| EvalError::Missing(field.to_string()))
    }
}

/// 求值（纯函数，可单测；架构 §3.4.2 的伪码逐行落地）。
pub fn eval(condition: &Condition, context: &EvalContext) -> Result<bool, EvalError> {
    match condition {
        // 短路：与架构伪码一致（`all` / `any` 本身即短路）
        Condition::And(args) => {
            for arg in args {
                if !eval(arg, context)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Or(args) => {
            for arg in args {
                if eval(arg, context)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not(inner) => Ok(!eval(inner, context)?),
        Condition::Cmp { field, cmp, value } => {
            let actual = context.resolve(field)?;
            compare(actual, *cmp, value, field)
        }
    }
}

fn compare(
    actual: &Value,
    cmp: CompareOp,
    expected: &Value,
    field: &str,
) -> Result<bool, EvalError> {
    match cmp {
        CompareOp::IsNull => Ok(actual.is_null()),
        CompareOp::NotNull => Ok(!actual.is_null()),
        CompareOp::Eq => Ok(json_eq(actual, expected)),
        CompareOp::Ne => Ok(!json_eq(actual, expected)),
        CompareOp::In | CompareOp::Nin => {
            let items = expected
                .as_array()
                .ok_or_else(|| EvalError::TypeMismatch(format!("{field}: in/nin 需要数组右值")))?;
            let hit = items.iter().any(|item| json_eq(actual, item));
            Ok(if cmp == CompareOp::In { hit } else { !hit })
        }
        CompareOp::Contains => match actual {
            Value::String(text) => {
                let needle = expected.as_str().ok_or_else(|| {
                    EvalError::TypeMismatch(format!("{field}: contains 需要字符串右值"))
                })?;
                Ok(text.contains(needle))
            }
            Value::Array(items) => Ok(items.iter().any(|item| json_eq(item, expected))),
            _ => Err(EvalError::TypeMismatch(format!(
                "{field}: contains 只能用于字符串或数组"
            ))),
        },
        CompareOp::Gt | CompareOp::Gte | CompareOp::Lt | CompareOp::Lte => {
            let left = actual
                .as_f64()
                .ok_or_else(|| EvalError::TypeMismatch(format!("{field}: 左值不是数字")))?;
            let right = expected
                .as_f64()
                .ok_or_else(|| EvalError::TypeMismatch(format!("{field}: 右值不是数字")))?;
            Ok(match cmp {
                CompareOp::Gt => left > right,
                CompareOp::Gte => left >= right,
                CompareOp::Lt => left < right,
                _ => left <= right,
            })
        }
    }
}

fn json_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => left == right,
    }
}

// ==================== 快照序列化 + graph_hash（L2） ====================

/// 快照格式版本（将来结构变更时靠它做兼容读）。
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// 快照里的节点（字段固定、顺序固定 —— canonical JSON 的前提）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotNode {
    /// 节点 key。
    pub key: String,
    /// 名称。
    pub name: String,
    /// 类型字符串。
    pub node_type: String,
    /// 审批人来源。
    pub assignee_type: Option<String>,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值（字符串保留 0.600 的精度，避免浮点序列化差异破坏 hash 稳定性）。
    pub approve_ratio: Option<String>,
    /// 一票否决。
    pub reject_veto: bool,
    /// 汇聚模式。
    pub join_mode: String,
    /// 分叉模式。
    pub fork_mode: String,
    /// 表单变量名。
    pub form_var_key: Option<String>,
    /// 主管层数。
    pub superior_levels: i16,
    /// 空审批人策略。
    pub empty_assignee: String,
    /// 自审策略。
    pub self_approval: String,
    /// 是否允许机器人。
    pub allow_robot: bool,
    /// 兜底审批人（用户 id，P3-03；`None` = 不兜底）。
    ///
    /// `skip_serializing_if` 是**必须**的：`graph_hash` 是快照 canonical JSON 的 sha256，
    /// 未配兜底的图（存量全部流程）序列化结果必须与 P3-02 逐字节一致 —— 否则
    /// 一次纯加字段的升级会把所有已发布流程的 hash 全改掉（版本比对 / 回归全部误报）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_user_id: Option<String>,
    /// 兜底角色（角色 id，P3-03；`None` = 不兜底）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_role_id: Option<String>,
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
    /// 画布 x。
    pub x: i32,
    /// 画布 y。
    pub y: i32,
    /// 排序。
    pub sort_order: i32,
    /// 扩展配置。
    pub config: Value,
    /// 可枚举审批人（按 kind + ref_id 排序）。
    pub assignees: Vec<SnapshotAssignee>,
}

/// 快照里的审批人引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAssignee {
    /// 来源类型。
    pub assignee_type: String,
    /// 目标 id。
    pub ref_id: String,
    /// 顺序。
    pub sort_order: i32,
}

/// 快照里的边。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEdge {
    /// 边 key。
    pub key: String,
    /// 起点。
    pub from: String,
    /// 终点。
    pub to: String,
    /// 条件 DSL。
    pub condition: Option<Value>,
    /// 条件标签。
    pub label: Option<String>,
    /// 优先级。
    pub priority: i32,
    /// 是否兜底。
    pub is_default: bool,
}

/// 发布快照（`biz_flow_def_version.graph_snapshot` 的结构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 格式版本。
    pub schema_version: u32,
    /// 节点（按 key 升序）。
    pub nodes: Vec<SnapshotNode>,
    /// 边（按 from, to, key 升序）。
    pub edges: Vec<SnapshotEdge>,
}

impl FlowGraph {
    /// 生成快照（**确定性**：节点按 key、边按 (from,to,key)、审批人按 (kind,ref_id) 排序）。
    ///
    /// 排序是 `graph_hash` 稳定性的前提：同一张图无论编辑器提交顺序如何，
    /// 序列化结果必须逐字节相同（验收 10：同图两次发布 hash 相同）。
    pub fn snapshot(&self) -> Snapshot {
        let mut nodes: Vec<SnapshotNode> = self
            .nodes
            .iter()
            .map(|node| {
                let mut assignees: Vec<SnapshotAssignee> = node
                    .assignees
                    .iter()
                    .map(|assignee| SnapshotAssignee {
                        assignee_type: assignee.kind.clone(),
                        ref_id: assignee.ref_id.clone(),
                        sort_order: assignee.sort_order,
                    })
                    .collect();
                assignees.sort_by(|left, right| {
                    (left.assignee_type.as_str(), left.ref_id.as_str())
                        .cmp(&(right.assignee_type.as_str(), right.ref_id.as_str()))
                });
                SnapshotNode {
                    key: node.key.clone(),
                    name: node.name.clone(),
                    node_type: node.node_type.as_str().to_string(),
                    assignee_type: node.assignee_type.map(|kind| kind.as_str().to_string()),
                    approval_mode: node.approval_mode.map(|mode| mode.as_str().to_string()),
                    approve_ratio: node.approve_ratio.map(|ratio| format!("{ratio:.3}")),
                    reject_veto: node.reject_veto,
                    join_mode: node.join_mode.as_str().to_string(),
                    fork_mode: node.fork_mode.as_str().to_string(),
                    form_var_key: node.form_var_key.clone(),
                    superior_levels: node.superior_levels,
                    empty_assignee: node.empty_assignee.clone(),
                    self_approval: node.self_approval.clone(),
                    allow_robot: node.allow_robot,
                    fallback_user_id: node.fallback_user_id.map(|id| id.to_string()),
                    fallback_role_id: node.fallback_role_id.map(|id| id.to_string()),
                    timeout_hours: node.timeout_hours,
                    timeout_action: node.timeout_action.clone(),
                    remind_before_hours: node.remind_before_hours,
                    max_remind_times: node.max_remind_times,
                    cc_on_approved: node.cc_on_approved,
                    x: node.x,
                    y: node.y,
                    sort_order: node.sort_order,
                    config: node.config.clone(),
                    assignees,
                }
            })
            .collect();
        nodes.sort_by(|left, right| left.key.cmp(&right.key));

        let mut edges: Vec<SnapshotEdge> = self
            .edges
            .iter()
            .map(|edge| SnapshotEdge {
                key: edge.key.clone(),
                from: edge.from.clone(),
                to: edge.to.clone(),
                condition: edge.condition.clone(),
                label: edge.label.clone(),
                priority: edge.priority,
                is_default: edge.is_default,
            })
            .collect();
        edges.sort_by(|left, right| {
            (&left.from, &left.to, &left.key).cmp(&(&right.from, &right.to, &right.key))
        });

        Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            nodes,
            edges,
        }
    }

    /// 快照的 canonical JSON（紧凑 + **递归按键字典序** + 数组已排序）。
    ///
    /// 不用 `serde_json::to_string`：`serde_json::Map` 是否保持插入序取决于
    /// `preserve_order` feature（本仓没开，但**依赖合并可能被别的 crate 打开**），
    /// 靠「库默认行为」保证 hash 稳定是脆的。这里自己递归拼，键序由排序决定，与 feature 无关。
    pub fn snapshot_json(&self) -> String {
        match serde_json::to_value(self.snapshot()) {
            Ok(value) => canonical_json(&value),
            Err(_) => "{}".to_string(),
        }
    }

    /// `graph_hash` = sha256(canonical JSON) 的小写十六进制（64 位）。
    pub fn graph_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = self.snapshot_json();
        let digest = Sha256::digest(canonical.as_bytes());
        let mut out = String::with_capacity(64);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// canonical JSON：对象键**递归升序**、无多余空白、数组保持既有顺序（调用方负责排序）。
///
/// 这是 `graph_hash` 与「快照自检」的唯一序列化入口 —— 任何绕过它的写法都会让
/// 「同一张图算出两个 hash」（验收 10 的反面）。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => match serde_json::to_string(text) {
            Ok(quoted) => out.push_str(&quoted),
            Err(_) => out.push_str("\"\""),
        },
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                match serde_json::to_string(key) {
                    Ok(quoted) => out.push_str(&quoted),
                    Err(_) => out.push_str("\"\""),
                }
                out.push(':');
                if let Some(item) = map.get(*key) {
                    write_canonical(item, out);
                }
            }
            out.push('}');
        }
    }
}

/// 反序列化快照（D2 的引擎从这里拿 L3 内存图）。
pub fn parse_snapshot(raw: &str) -> Result<Snapshot, String> {
    let snapshot: Snapshot =
        serde_json::from_str(raw).map_err(|error| format!("快照 JSON 解析失败：{error}"))?;
    if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(format!(
            "快照格式版本 {} 与当前引擎（{SNAPSHOT_SCHEMA_VERSION}）不一致",
            snapshot.schema_version
        ));
    }
    Ok(snapshot)
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    /// 造一条纯链：n0 → n1 → … → n{count-1}，首节点 START、末节点 END。
    fn chain(count: usize) -> FlowGraph {
        let mut nodes = Vec::with_capacity(count);
        let mut edges = Vec::with_capacity(count.saturating_sub(1));
        for index in 0..count {
            let node_type = if index == 0 {
                NodeType::Start
            } else if index + 1 == count {
                NodeType::End
            } else {
                NodeType::Approval
            };
            let mut node = NodeSpec::new(format!("n{index}"), format!("节点{index}"), node_type);
            if node_type == NodeType::Approval {
                node.assignee_type = Some(AssigneeKind::Initiator);
            }
            nodes.push(node);
            if index > 0 {
                edges.push(EdgeSpec::new(
                    format!("e{index}"),
                    format!("n{}", index - 1),
                    format!("n{index}"),
                ));
            }
        }
        FlowGraph { nodes, edges }
    }

    /// 合法的最小图：START → APPROVAL → END。
    fn minimal() -> FlowGraph {
        let mut start = NodeSpec::new("n_start", "开始", NodeType::Start);
        start.assignee_type = None;
        let mut approve = NodeSpec::new("n_approve", "主管审批", NodeType::Approval);
        approve.assignee_type = Some(AssigneeKind::InitiatorSuperior);
        FlowGraph {
            nodes: vec![
                start,
                approve,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_approve"),
                EdgeSpec::new("e2", "n_approve", "n_end"),
            ],
        }
    }

    fn rules(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|issue| issue.rule).collect()
    }

    // ---------- 环检测（4 种 + 性能） ----------

    #[test]
    fn cycle_1_chain_of_200_has_no_cycle() {
        let graph = chain(200);
        let started = Instant::now();
        let cycle = graph.find_cycle();
        let elapsed = started.elapsed();
        assert!(cycle.is_none(), "纯链不该有环");
        assert!(
            elapsed.as_millis() < 50,
            "200 节点链的环检测必须 < 50 ms（实测 {:?}）",
            elapsed
        );
        println!(
            "[R3] 纯链 200 节点 → None，耗时 {:?}（{} µs）",
            elapsed,
            elapsed.as_micros()
        );
    }

    #[test]
    fn cycle_2_back_edge_returns_closed_path() {
        let mut graph = chain(200);
        // 加一条回边 n199 → n5（架构 §8 证据 B 的那条回边）
        graph.edges.push(EdgeSpec::new("e_back", "n199", "n5"));
        let cycle = graph.find_cycle().expect("必须检出环");
        assert!(
            cycle.len() >= 2,
            "闭合路径至少两个节点（首尾相同）：{cycle:?}"
        );
        assert_eq!(cycle.first(), cycle.last(), "路径必须首尾相同：{cycle:?}");
        println!(
            "[R3] 加回边后闭合路径（{} 跳）：{}",
            cycle.len(),
            cycle.join(" → ")
        );
    }

    #[test]
    fn cycle_3_self_loop_detected() {
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("a", "A", NodeType::Start),
                NodeSpec::new("b", "B", NodeType::Approval),
            ],
            edges: vec![EdgeSpec::new("e1", "a", "b"), EdgeSpec::new("e2", "b", "b")],
        };
        let cycle = graph.find_cycle().expect("自环必须检出");
        assert_eq!(cycle, vec!["b".to_string(), "b".to_string()]);
    }

    #[test]
    fn cycle_4_diamond_is_not_a_cycle() {
        let mut condition = NodeSpec::new("n_cond", "条件", NodeType::Condition);
        condition.join_mode = GatewayMode::And;
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                condition,
                {
                    let mut node = NodeSpec::new("n_a", "A 分支", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                {
                    let mut node = NodeSpec::new("n_b", "B 分支", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_cond"),
                EdgeSpec::new("e2", "n_cond", "n_a"),
                {
                    let mut edge = EdgeSpec::new("e3", "n_cond", "n_b");
                    edge.is_default = true;
                    edge
                },
                EdgeSpec::new("e4", "n_a", "n_end"),
                EdgeSpec::new("e5", "n_b", "n_end"),
            ],
        };
        assert!(graph.find_cycle().is_none(), "菱形分支不是环");
    }

    // ---------- 拓扑排序 ----------

    #[test]
    fn toposort_orders_chain() {
        let graph = chain(5);
        let order = graph.topological_order().expect("链必须可拓扑排序");
        assert_eq!(order, vec!["n0", "n1", "n2", "n3", "n4"]);
        assert!(graph.cycle_nodes().is_empty());
    }

    #[test]
    fn toposort_returns_none_on_cycle_and_lists_cycle_nodes() {
        let mut graph = chain(4);
        graph.edges.push(EdgeSpec::new("e_back", "n3", "n1"));
        assert!(graph.topological_order().is_none(), "有环时 Kahn 必须失败");
        let cyc = graph.cycle_nodes();
        assert!(cyc.contains(&"n1".to_string()), "环上节点：{cyc:?}");
        assert!(cyc.contains(&"n2".to_string()), "环上节点：{cyc:?}");
    }

    // ---------- 12 条校验：正例 ----------

    #[test]
    fn validate_ok_on_minimal_graph() {
        let issues = minimal().validate();
        assert!(issues.is_empty(), "最小合法图不该有不通过项：{issues:?}");
    }

    #[test]
    fn validate_ok_on_condition_diamond() {
        let mut cond = NodeSpec::new("n_cond", "金额判断", NodeType::Condition);
        cond.join_mode = GatewayMode::Or;
        let mut approve_a = NodeSpec::new("n_a", "财务审批", NodeType::Approval);
        approve_a.assignee_type = Some(AssigneeKind::Role);
        approve_a.assignees = vec![AssigneeRef {
            kind: "ROLE".into(),
            ref_id: uuid_like(7),
            sort_order: 0,
        }];
        let mut merge = NodeSpec::new("n_merge", "汇合审批", NodeType::Approval);
        merge.assignee_type = Some(AssigneeKind::Initiator);
        merge.join_mode = GatewayMode::Or;
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                cond,
                approve_a,
                {
                    let mut node = NodeSpec::new("n_b", "主管审批", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::InitiatorSuperior);
                    node
                },
                merge,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_cond"),
                {
                    let mut edge = EdgeSpec::new("e2", "n_cond", "n_a");
                    edge.condition =
                        Some(json!({"op":"cmp","field":"form.amount","cmp":"gte","value":10000}));
                    edge
                },
                {
                    let mut edge = EdgeSpec::new("e3", "n_cond", "n_b");
                    edge.is_default = true;
                    edge
                },
                EdgeSpec::new("e4", "n_a", "n_merge"),
                EdgeSpec::new("e5", "n_b", "n_merge"),
                EdgeSpec::new("e6", "n_merge", "n_end"),
            ],
        };
        let issues = graph.validate();
        assert!(issues.is_empty(), "合法菱形不该有不通过项：{issues:?}");
    }

    // ---------- 12 条校验：反例（每条一条） ----------

    #[test]
    fn r1_rejects_zero_or_two_starts() {
        let mut graph = minimal();
        graph.nodes[0].node_type = NodeType::Approval;
        graph.nodes[0].assignee_type = Some(AssigneeKind::Initiator);
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R1"), "{issues:?}");
        assert_eq!(issues[0].code, error_code::FLOW_GRAPH_STRUCTURE);
        println!("[R1 反例] {}", issues[0].message);

        let mut graph = minimal();
        graph
            .nodes
            .push(NodeSpec::new("n_start2", "开始2", NodeType::Start));
        graph
            .edges
            .push(EdgeSpec::new("e9", "n_start2", "n_approve"));
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R1"), "{issues:?}");
    }

    #[test]
    fn r2_rejects_missing_end() {
        let mut graph = minimal();
        graph.nodes.pop();
        graph.edges.pop();
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R2"), "{issues:?}");
        let issue = issues.iter().find(|issue| issue.rule == "R2").unwrap();
        assert_eq!(issue.code, error_code::FLOW_GRAPH_STRUCTURE);
        println!("[R2 反例] {}", issue.message);
    }

    #[test]
    fn r3_rejects_cycle_and_names_every_node() {
        let mut graph = chain(5);
        graph.edges.push(EdgeSpec::new("e_back", "n4", "n1"));
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R3")
            .expect("必须有环");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_CYCLE);
        for key in ["n1", "n2", "n3", "n4"] {
            assert!(
                issue.message.contains(key),
                "文案必须点名 {key}：{}",
                issue.message
            );
        }
        assert!(issue.message.contains(" → "), "必须给出闭合路径");
        println!("[R3 反例] {}", issue.message);
    }

    #[test]
    fn r4_rejects_start_with_inbound_and_end_with_outbound() {
        let mut graph = minimal();
        graph
            .edges
            .push(EdgeSpec::new("e3", "n_approve", "n_start"));
        graph.edges.push(EdgeSpec::new("e4", "n_end", "n_approve"));
        let issues = graph.validate();
        let degree_issues: Vec<&Issue> = issues.iter().filter(|issue| issue.rule == "R4").collect();
        assert_eq!(degree_issues.len(), 2, "{issues:?}");
        assert!(
            degree_issues
                .iter()
                .all(|issue| issue.code == error_code::FLOW_GRAPH_DEGREE)
        );
        println!("[R4 反例] {}", degree_issues[0].message);
    }

    #[test]
    fn r5_rejects_node_without_out_edge() {
        let mut graph = minimal();
        graph.nodes.push({
            let mut node = NodeSpec::new("n_orphan", "孤儿审批", NodeType::Approval);
            node.assignee_type = Some(AssigneeKind::Initiator);
            node
        });
        graph
            .edges
            .push(EdgeSpec::new("e9", "n_approve", "n_orphan"));
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R5"), "{issues:?}");
        let issue = issues.iter().find(|issue| issue.rule == "R5").unwrap();
        assert_eq!(issue.code, error_code::FLOW_GRAPH_DEGREE);
        println!("[R5 反例] {}", issue.message);
    }

    #[test]
    fn r6_rejects_approval_with_two_out_edges() {
        let mut graph = minimal();
        graph
            .nodes
            .push(NodeSpec::new("n_end2", "结束2", NodeType::End));
        graph.edges.push(EdgeSpec::new("e3", "n_approve", "n_end2"));
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R6")
            .expect("必须是 R6");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_DEGREE);
        assert!(issue.message.contains("n_approve"), "{}", issue.message);
        println!("[R6 反例] {}", issue.message);
    }

    #[test]
    fn r7_rejects_condition_with_single_out_edge() {
        let mut graph = minimal();
        graph.nodes[1].node_type = NodeType::Condition;
        graph.nodes[1].assignee_type = None;
        graph.edges[1].is_default = true;
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R7")
            .expect("必须是 R7");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_DEGREE);
        println!("[R7 反例] {}", issue.message);
    }

    #[test]
    fn r8_rejects_xor_branches_joining_with_and() {
        let mut cond = NodeSpec::new("n_cond", "金额判断", NodeType::Condition);
        cond.join_mode = GatewayMode::Or;
        let mut merge = NodeSpec::new("n_merge", "汇合审批", NodeType::Approval);
        merge.assignee_type = Some(AssigneeKind::Initiator);
        merge.join_mode = GatewayMode::And; // ← 条件分支却用 AND 汇聚
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                cond,
                {
                    let mut node = NodeSpec::new("n_a", "A", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                {
                    let mut node = NodeSpec::new("n_b", "B", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                merge,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_cond"),
                {
                    let mut edge = EdgeSpec::new("e2", "n_cond", "n_a");
                    edge.condition =
                        Some(json!({"op":"cmp","field":"form.amount","cmp":"gt","value":1}));
                    edge
                },
                {
                    let mut edge = EdgeSpec::new("e3", "n_cond", "n_b");
                    edge.is_default = true;
                    edge
                },
                EdgeSpec::new("e4", "n_a", "n_merge"),
                EdgeSpec::new("e5", "n_b", "n_merge"),
                EdgeSpec::new("e6", "n_merge", "n_end"),
            ],
        };
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R8")
            .expect("必须是 R8");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_JOIN_MODE);
        assert!(issue.message.contains("OR"), "{}", issue.message);
        println!("[R8 反例] {}", issue.message);
    }

    #[test]
    fn r8_rejects_mixed_sources() {
        // 两个不同的条件网关汇到同一个节点 → 混流
        let mut cond1 = NodeSpec::new("n_cond1", "条件1", NodeType::Condition);
        cond1.join_mode = GatewayMode::Or;
        let mut cond2 = NodeSpec::new("n_cond2", "条件2", NodeType::Condition);
        cond2.join_mode = GatewayMode::Or;
        let mut merge = NodeSpec::new("n_merge", "汇合", NodeType::Approval);
        merge.assignee_type = Some(AssigneeKind::Initiator);
        merge.join_mode = GatewayMode::Or;
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                cond1,
                cond2,
                merge,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_cond1"),
                {
                    let mut edge = EdgeSpec::new("e2", "n_cond1", "n_cond2");
                    edge.condition =
                        Some(json!({"op":"cmp","field":"form.amount","cmp":"gt","value":1}));
                    edge
                },
                {
                    let mut edge = EdgeSpec::new("e3", "n_cond1", "n_merge");
                    edge.is_default = true;
                    edge
                },
                {
                    let mut edge = EdgeSpec::new("e4", "n_cond2", "n_merge");
                    edge.is_default = true;
                    edge
                },
                {
                    let mut edge = EdgeSpec::new("e5", "n_cond2", "n_merge");
                    edge.condition =
                        Some(json!({"op":"cmp","field":"form.amount","cmp":"lte","value":1}));
                    edge
                },
                EdgeSpec::new("e6", "n_merge", "n_end"),
            ],
        };
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R8"), "{issues:?}");
        println!(
            "[R8 混流反例] {}",
            issues.iter().find(|i| i.rule == "R8").unwrap().message
        );
    }

    #[test]
    fn r8_allows_a_bypass_branch_rejoining_the_fork_itself() {
        // 「有则加一道复核、无则直过」：n_cond 的一支经 n_extra 再汇合，另一支**直接**汇合。
        // 汇聚节点 n_merge 的一条入边直接来自网关本身 —— 这是 BPMN 里最常见的写法，
        // 绝不能判成「上游没有分支网关」（R8 假阳性，P2-2b 实测踩到）。
        let build = |merge_join: GatewayMode| {
            let mut cond = NodeSpec::new("n_cond", "超信用额度判定", NodeType::Condition);
            cond.join_mode = GatewayMode::Or;
            let mut extra = NodeSpec::new("n_extra", "财务复核", NodeType::Approval);
            extra.assignee_type = Some(AssigneeKind::Initiator);
            let mut merge = NodeSpec::new("n_merge", "金额分级①", NodeType::Condition);
            merge.join_mode = merge_join;
            FlowGraph {
                nodes: vec![
                    NodeSpec::new("n_start", "开始", NodeType::Start),
                    cond,
                    extra,
                    merge,
                    NodeSpec::new("n_end", "结束", NodeType::End),
                ],
                edges: vec![
                    EdgeSpec::new("e1", "n_start", "n_cond"),
                    {
                        let mut edge = EdgeSpec::new("e2", "n_cond", "n_extra");
                        edge.condition = Some(json!({
                            "op": "cmp", "field": "form.over_credit_limit", "cmp": "eq", "value": true
                        }));
                        edge
                    },
                    {
                        let mut edge = EdgeSpec::new("e3", "n_cond", "n_merge");
                        edge.is_default = true;
                        edge
                    },
                    EdgeSpec::new("e4", "n_extra", "n_merge"),
                    EdgeSpec::new("e5", "n_merge", "n_end"),
                ],
            }
        };

        // ① 条件分支 + OR 汇聚 = 合法（这条以前被判成「上游没有分支网关」）
        let issues = build(GatewayMode::Or).validate();
        assert!(
            !rules(&issues).contains(&"R8"),
            "分支直连汇聚节点是合法建模，不该报 R8：{issues:#?}"
        );

        // ② 同样的图改用 AND 汇聚 —— 仍必须拦住（条件分支只有一支会激活，AND 会永久卡死）
        let issues = build(GatewayMode::And).validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R8")
            .expect("条件分支用 AND 汇聚必须报 R8");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_JOIN_MODE);
        assert!(issue.message.contains("OR"), "{}", issue.message);
        println!("[R8 直连汇聚·AND 反例] {}", issue.message);
    }

    #[test]
    fn r9_rejects_bad_ratio_and_missing_mode() {
        // ① RATIO 越界
        let mut graph = minimal();
        graph.nodes[1].node_type = NodeType::Countersign;
        graph.nodes[1].approval_mode = Some(ApprovalMode::Ratio);
        graph.nodes[1].approve_ratio = Some(1.5);
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R9")
            .expect("必须是 R9");
        assert_eq!(issue.code, error_code::FLOW_NODE_ASSIGNEE_MISSING);
        println!("[R9 反例①] {}", issue.message);

        // ② COUNTERSIGN 没选模式
        let mut graph = minimal();
        graph.nodes[1].node_type = NodeType::Countersign;
        graph.nodes[1].approval_mode = None;
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R9"), "{issues:?}");
        println!(
            "[R9 反例②] {}",
            issues
                .iter()
                .find(|issue| issue.rule == "R9")
                .unwrap()
                .message
        );
    }

    /// P3-06 ①：`APPROVAL` 不填模式 = 合法（等价或签），选了 RATIO 就必须带 (0,1] 阈值。
    #[test]
    fn r9_approval_allows_null_mode_but_ratio_needs_threshold() {
        // ① 不填模式 → 不报 R9（历史规则只对 COUNTERSIGN 强制）
        let mut graph = minimal();
        assert_eq!(graph.nodes[1].node_type, NodeType::Approval);
        graph.nodes[1].approval_mode = None;
        let issues = graph.validate();
        assert!(
            !rules(&issues).contains(&"R9"),
            "APPROVAL 不填模式必须放行（NULL = 或签）：{issues:#?}"
        );

        // ② 显式 RATIO 但没填比例 → 必须报 R9（以前只有会签判，审批节点会漏）
        graph.nodes[1].approval_mode = Some(ApprovalMode::Ratio);
        graph.nodes[1].approve_ratio = None;
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R9")
            .expect("审批节点选按比例但没填阈值必须报 R9");
        assert!(
            issue.message.contains("审批节点") && issue.message.contains("按比例"),
            "文案要说明是审批节点的按比例：{}",
            issue.message
        );
        println!("[R9·审批节点按比例无阈值] {}", issue.message);

        // ③ 比例越界 → R9；补齐合法比例 → 通过
        graph.nodes[1].approve_ratio = Some(1.5);
        assert!(rules(&graph.validate()).contains(&"R9"));
        graph.nodes[1].approve_ratio = Some(0.6);
        assert!(
            !rules(&graph.validate()).contains(&"R9"),
            "补齐 (0,1] 的比例后不该再报 R9"
        );

        // ④ 会签节点不填模式 → 仍必须报 R9（历史规则不放松）
        let mut graph = minimal();
        graph.nodes[1].node_type = NodeType::Countersign;
        graph.nodes[1].approval_mode = None;
        assert!(rules(&graph.validate()).contains(&"R9"));
    }

    /// P3-06 ②：引擎兜底口径 —— `NULL` / 空串 / 非法值一律 = 或签。
    #[test]
    fn approval_mode_effective_defaults_to_any() {
        assert_eq!(ApprovalMode::effective(None), ApprovalMode::Any);
        assert_eq!(ApprovalMode::effective(Some("")), ApprovalMode::Any);
        assert_eq!(ApprovalMode::effective(Some("   ")), ApprovalMode::Any);
        assert_eq!(ApprovalMode::effective(Some("any")), ApprovalMode::Any);
        assert_eq!(ApprovalMode::effective(Some("ALL")), ApprovalMode::All);
        assert_eq!(ApprovalMode::effective(Some("ratio")), ApprovalMode::Ratio);
        // 库里 CHECK 保证不会出现非法值；真出现了也放宽到或签，不要卡死流程
        assert_eq!(ApprovalMode::effective(Some("MAJORITY")), ApprovalMode::Any);
    }

    /// P3-06 ③：迁移口径纯函数 —— 候选人数 ≥2 冻结 ALL，其余 ANY。
    #[test]
    fn freeze_sign_mode_follows_candidate_count() {
        assert_eq!(freeze_sign_mode(Some(3)), ApprovalMode::All);
        assert_eq!(freeze_sign_mode(Some(2)), ApprovalMode::All);
        // 0 / 1 人时 ALL 与 ANY 判定等价（0 人自动通过、1 人签完即通过）
        assert_eq!(freeze_sign_mode(Some(1)), ApprovalMode::Any);
        assert_eq!(freeze_sign_mode(Some(0)), ApprovalMode::Any);
        // 算不出来（FORM_VAR / EXPRESSION / 没配来源）不猜更严的值
        assert_eq!(freeze_sign_mode(None), ApprovalMode::Any);
    }

    #[test]
    fn r10_rejects_unknown_field_and_deep_nesting() {
        // ① 未知字段（`secret.*` 不在白名单）
        let mut graph = minimal();
        graph.edges[0].condition =
            Some(json!({"op":"cmp","field":"secret.amount","cmp":"eq","value":1}));
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R10")
            .expect("必须是 R10");
        assert_eq!(issue.code, error_code::FLOW_CONDITION_INVALID);
        println!("[R10 反例] {}", issue.message);

        // ② 嵌套超过 8 层
        let mut nested = json!({"op":"cmp","field":"form.amount","cmp":"eq","value":1});
        for _ in 0..9 {
            nested = json!({"op":"not","args":[nested]});
        }
        let mut graph = minimal();
        graph.edges[0].condition = Some(nested);
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R10")
            .expect("必须是 R10");
        assert!(issue.message.contains("层"), "{}", issue.message);
    }

    #[test]
    fn r11_rejects_unreachable_node() {
        let mut graph = minimal();
        graph.nodes.push({
            let mut node = NodeSpec::new("n_detached", "断开审批", NodeType::Approval);
            node.assignee_type = Some(AssigneeKind::Initiator);
            node
        });
        graph
            .nodes
            .push(NodeSpec::new("n_end2", "结束2", NodeType::End));
        graph
            .edges
            .push(EdgeSpec::new("e9", "n_detached", "n_end2"));
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R11")
            .expect("必须是 R11");
        assert!(issue.message.contains("n_detached"), "{}", issue.message);
        println!("[R11 反例] {}", issue.message);
    }

    #[test]
    fn r12_rejects_node_that_cannot_reach_end() {
        // START → A → B，B 没有出边也不是 END ⇒ B 走不到结束
        let mut graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                {
                    let mut node = NodeSpec::new("n_a", "A", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                {
                    let mut node = NodeSpec::new("n_b", "B", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_a"),
                EdgeSpec::new("e2", "n_a", "n_b"),
                EdgeSpec::new("e3", "n_b", "n_a"), // 死循环（环）也要能同时报出来
            ],
        };
        let issues = graph.validate();
        assert!(
            rules(&issues).contains(&"R3"),
            "自环必须同时被 R3 检出：{issues:?}"
        );
        assert!(
            rules(&issues).contains(&"R12"),
            "B 走不到 END 必须被 R12 检出：{issues:?}"
        );
        println!(
            "[R12 反例] {}",
            issues
                .iter()
                .find(|issue| issue.rule == "R12")
                .unwrap()
                .message
        );

        // 干净的断链：去掉回边，B 只有「走不到 END」这一个问题
        graph.edges.pop();
        let issues = graph.validate();
        let r12: Vec<&Issue> = issues.iter().filter(|issue| issue.rule == "R12").collect();
        assert!(
            r12.iter().any(|issue| issue.message.contains("n_b")),
            "文案必须点名走不到结束的节点 n_b：{:?}",
            issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            r12.iter()
                .all(|issue| issue.code == error_code::FLOW_GRAPH_STRUCTURE)
        );
    }

    #[test]
    fn r13_rejects_missing_default_edge() {
        let mut cond = NodeSpec::new("n_cond", "条件", NodeType::Condition);
        cond.join_mode = GatewayMode::Or;
        let graph = FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                cond,
                {
                    let mut node = NodeSpec::new("n_a", "A", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                {
                    let mut node = NodeSpec::new("n_b", "B", NodeType::Approval);
                    node.assignee_type = Some(AssigneeKind::Initiator);
                    node
                },
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_cond"),
                {
                    let mut edge = EdgeSpec::new("e2", "n_cond", "n_a");
                    edge.condition =
                        Some(json!({"op":"cmp","field":"form.amount","cmp":"gt","value":1}));
                    edge
                },
                // e3 无条件也不是兜底
                EdgeSpec::new("e3", "n_cond", "n_b"),
                EdgeSpec::new("e4", "n_a", "n_end"),
                EdgeSpec::new("e5", "n_b", "n_end"),
            ],
        };
        let issues = graph.validate();
        let default_issues: Vec<&Issue> =
            issues.iter().filter(|issue| issue.rule == "R13").collect();
        assert!(!default_issues.is_empty(), "{issues:?}");
        assert!(
            default_issues
                .iter()
                .all(|issue| issue.code == error_code::FLOW_GRAPH_DEFAULT_EDGE)
        );
        println!("[R13 反例] {}", default_issues[0].message);
    }

    #[test]
    fn r14_rejects_empty_assignee() {
        let mut graph = minimal();
        graph.nodes[1].assignee_type = None;
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R14")
            .expect("必须是 R14");
        assert_eq!(issue.code, error_code::FLOW_NODE_ASSIGNEE_MISSING);
        assert!(issue.message.contains("n_approve"), "{}", issue.message);
        println!("[R14 反例] {}", issue.message);

        // ROLE 但一条 assignee 都没有
        let mut graph = minimal();
        graph.nodes[1].assignee_type = Some(AssigneeKind::Role);
        let issues = graph.validate();
        assert!(rules(&issues).contains(&"R14"), "{issues:?}");
    }

    /// E2 缺陷 1：只给 `assignees`（不填 `assignee_type`）必须通过 R14。
    ///
    /// 实测症状：PUT graph 成功、`biz_flow_node_assignee` 落 1 行，但 PUBLISH 报
    /// 65015「节点 n1 未配置审批人」——`assignee_type` 落 NULL，而 R14 只看它。
    #[test]
    fn r14_accepts_assignees_without_assignee_type() {
        let mut graph = minimal();
        graph.nodes[1].assignee_type = None;
        graph.nodes[1].assignees = vec![AssigneeRef {
            kind: "USER".to_string(),
            ref_id: "6f1d0f3e-0000-4000-8000-000000000001".to_string(),
            sort_order: 0,
        }];
        let issues = graph.validate();
        assert!(
            !rules(&issues).contains(&"R14"),
            "只给 assignees 不该报 R14：{issues:?}"
        );
    }

    /// E2 反例：两个都不给，仍然必须报 R14 / 65015（不能把校验放宽成恒通过）。
    #[test]
    fn r14_still_rejects_when_both_missing() {
        let mut graph = minimal();
        graph.nodes[1].assignee_type = None;
        graph.nodes[1].assignees.clear();
        let issue = graph
            .validate()
            .into_iter()
            .find(|issue| issue.rule == "R14")
            .expect("必须仍然报 R14");
        assert_eq!(issue.code, error_code::FLOW_NODE_ASSIGNEE_MISSING);
    }

    /// E2 反例：`assignees` 里的 `kind` 全都解析不出来时，不能算「配了审批人」。
    #[test]
    fn r14_rejects_unparsable_assignee_kind() {
        let mut graph = minimal();
        graph.nodes[1].assignee_type = None;
        graph.nodes[1].assignees = vec![AssigneeRef {
            kind: "SOMEONE".to_string(),
            ref_id: "x".to_string(),
            sort_order: 0,
        }];
        assert!(rules(&graph.validate()).contains(&"R14"));
    }

    #[test]
    fn r15_rejects_oversized_graph() {
        let mut graph = chain(MAX_NODES + 1);
        graph.edges.clear();
        let issues = graph.validate();
        let issue = issues
            .iter()
            .find(|issue| issue.rule == "R15")
            .expect("必须是 R15");
        assert_eq!(issue.code, error_code::FLOW_GRAPH_TOO_LARGE);
        println!("[R15 反例] {}", issue.message);
    }

    // ---------- 条件 DSL ----------

    #[test]
    fn dsl_parses_nested_expression() {
        let raw = json!({
            "op":"and",
            "args":[
                {"op":"cmp","field":"form.amount","cmp":"gte","value":10000},
                {"op":"cmp","field":"form.priority","cmp":"in","value":["P0","P1"]},
                {"op":"not","args":[{"op":"cmp","field":"initiator.dept_id","cmp":"in","value":[uuid_like(1)]}]}
            ]
        });
        let condition = parse_condition(&raw).expect("合法表达式必须解析通过");
        if let Condition::And(args) = &condition {
            assert_eq!(args.len(), 3);
        } else {
            panic!("顶层必须是 and");
        }
    }

    #[test]
    fn dsl_rejects_unknown_field_object_literal_and_bad_cmp() {
        let unknown = json!({"op":"cmp","field":"form.amount; DROP TABLE","cmp":"eq","value":1});
        assert!(parse_condition(&unknown).is_err());

        let object_literal = json!({"op":"cmp","field":"form.amount","cmp":"eq","value":{"a":1}});
        assert!(parse_condition(&object_literal).is_err());

        let bad_cmp = json!({"op":"cmp","field":"form.amount","cmp":"like","value":1});
        assert!(parse_condition(&bad_cmp).is_err());

        let bad_op = json!({"op":"eval","args":[]});
        assert!(parse_condition(&bad_op).is_err());

        let too_many_args = json!({
            "op":"and",
            "args": (0..17).map(|_| json!({"op":"cmp","field":"form.amount","cmp":"eq","value":1})).collect::<Vec<_>>()
        });
        assert!(parse_condition(&too_many_args).is_err());
    }

    #[test]
    fn dsl_node_budget_is_enforced() {
        // 65 个 cmp 节点（超过 64 上限）
        let args: Vec<Value> = (0..65)
            .map(|_| json!({"op":"cmp","field":"form.amount","cmp":"eq","value":1}))
            .collect();
        let raw = json!({"op":"and","args":args});
        let error = parse_condition(&raw).expect_err("必须因节点数超限被拒");
        assert!(error.contains("64") || error.contains("args"), "{error}");
    }

    #[test]
    fn dsl_eval_works() {
        let raw = json!({
            "op":"and",
            "args":[
                {"op":"cmp","field":"form.amount","cmp":"gte","value":10000},
                {"op":"cmp","field":"task.priority","cmp":"in","value":["P0","P1"]}
            ]
        });
        let condition = parse_condition(&raw).unwrap();
        let mut context = EvalContext::default();
        context.form.insert("amount".into(), json!(12000));
        context.task.insert("priority".into(), json!("P0"));
        assert!(eval(&condition, &context).unwrap());

        context.form.insert("amount".into(), json!(99));
        assert!(!eval(&condition, &context).unwrap());

        // 字段缺失 → Missing（不是 false）
        context.form.remove("amount");
        assert_eq!(
            eval(&condition, &context),
            Err(EvalError::Missing("form.amount".to_string()))
        );
    }

    // ---------- 快照 / hash ----------

    #[test]
    fn snapshot_is_deterministic_and_hash_stable() {
        let graph = minimal();
        let hash_a = graph.graph_hash();
        let hash_b = graph.graph_hash();
        assert_eq!(hash_a, hash_b, "同图两次算 hash 必须相同");
        assert_eq!(hash_a.len(), 64);

        // 节点 / 边的提交顺序打乱 → 同一个 hash
        let mut shuffled = graph.clone();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        assert_eq!(shuffled.graph_hash(), hash_a, "顺序不影响 hash");

        // 改一个字节 → hash 变
        let mut changed = graph.clone();
        changed.nodes[1].name = "主管审批（改名）".to_string();
        assert_ne!(changed.graph_hash(), hash_a);
        println!("[hash] minimal = {hash_a}");
    }

    #[test]
    fn snapshot_round_trips() {
        let graph = minimal();
        let text = graph.snapshot_json();
        let parsed = parse_snapshot(&text).expect("快照必须能反序列化");
        assert_eq!(parsed.nodes.len(), 3);
        assert_eq!(parsed.edges.len(), 2);
        assert_eq!(parsed.schema_version, SNAPSHOT_SCHEMA_VERSION);
        // canonical JSON：键名有序（本模块的 canonical 序列化器保证），不含空格
        assert!(!text.contains(' '), "紧凑 JSON 不该有空格");
        let edges_index = text.find("\"edges\"").unwrap();
        let nodes_index = text.find("\"nodes\"").unwrap();
        let schema_index = text.find("\"schema_version\"").unwrap();
        assert!(
            edges_index < nodes_index && nodes_index < schema_index,
            "键名必须字典序：edges < nodes < schema_version"
        );
        println!("[snapshot] 前 120 字节：{}", &text[..text.len().min(120)]);
    }

    /// 红线自检：本模块**不允许出现递归 CTE**（架构 §8 / D20）。
    ///
    /// 用 `include_str!` 把本模块的四个源文件编进来做文本断言：`RECURSIVE` 只允许出现在
    /// 解释「为什么不用它」的注释里。这条测试比人工 `grep` 更硬 —— 它随 `cargo test` 常驻，
    /// 谁将来图省事写一条 `WITH RECURSIVE` 判环，这里立刻红。
    #[test]
    fn module_sources_have_no_cte_cycle() {
        let sources = [
            ("graph.rs", include_str!("graph.rs")),
            ("model.rs", include_str!("model.rs")),
            ("handler.rs", include_str!("handler.rs")),
            ("dto.rs", include_str!("dto.rs")),
        ];
        // 探针字符串拆开写：否则这条测试自己的源码会命中自己（`contains("RECURSIVE")` 那一行
        // 既不是注释、又含关键词）
        let needle = ["RECUR", "SIVE"].concat();
        let mut offenders: Vec<String> = Vec::new();
        for (name, text) in sources {
            for (index, line) in text.lines().enumerate() {
                if !line.to_uppercase().contains(&needle) {
                    continue;
                }
                let trimmed = line.trim_start();
                let is_comment = trimmed.starts_with("//")
                    || trimmed.starts_with('*')
                    || trimmed.starts_with("/*");
                if !is_comment {
                    offenders.push(format!("{name}:{}", index + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "本模块禁止递归 CTE（架构 §8 实测 300 秒未结束 / D20）；违规位置：{offenders:?}"
        );
        println!("[环检测] 4 个源文件里 {needle} 只出现在注释中（非注释命中 0）");
    }

    /// 造一个稳定的假 UUID 字符串（同一序号永远同一值）。
    fn uuid_like(seed: u32) -> String {
        format!("00000000-0000-0000-0000-{seed:012}")
    }

    /// 节点上的运行期策略（空审批人 / 自审 / 机器人 / 超时）必须进快照 ——
    /// D2 的引擎只读快照，漏掉任何一列都会让「发布时配的超时」在运行时凭空消失。
    #[test]
    fn snapshot_carries_runtime_policy_fields() {
        let mut graph = minimal();
        if let Some(node) = graph.nodes.iter_mut().find(|node| node.key == "n_approve") {
            node.empty_assignee = "ERROR".to_string();
            node.self_approval = "FORBID".to_string();
            node.allow_robot = true;
            node.timeout_hours = Some(24);
            node.timeout_action = "AUTO_REJECT".to_string();
            node.max_remind_times = 5;
            node.cc_on_approved = true;
        }
        let snapshot = graph.snapshot();
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.key == "n_approve")
            .unwrap();
        assert_eq!(node.timeout_hours, Some(24));
        assert_eq!(node.timeout_action, "AUTO_REJECT");
        assert_eq!(node.empty_assignee, "ERROR");
        assert!(node.allow_robot);
        assert!(node.cc_on_approved);

        let parsed = parse_snapshot(&graph.snapshot_json()).expect("快照必须可反序列化");
        let again = parsed
            .nodes
            .iter()
            .find(|node| node.key == "n_approve")
            .unwrap();
        assert_eq!(again.self_approval, "FORBID");
        assert_eq!(again.max_remind_times, 5);
    }
}

// ==================== R16 / EXPRESSION 单测（批次 D · P1-A2） ====================

/// R16（发布期表达式校验）的单测。
///
/// 独立模块：既有的 R1–R15 用例一字不动。
#[cfg(test)]
mod expression_tests {
    use super::*;
    use serde_json::json;

    /// START → 审批（`assignee_type=EXPRESSION`，config 由调用方给）→ END。
    fn graph_with_expression(config: Value) -> FlowGraph {
        let start = NodeSpec::new("n_start", "开始", NodeType::Start);
        let mut approve = NodeSpec::new("n_approve", "财务审批", NodeType::Approval);
        approve.assignee_type = Some(AssigneeKind::Expression);
        approve.config = config;
        FlowGraph {
            nodes: vec![
                start,
                approve,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_approve"),
                EdgeSpec::new("e2", "n_approve", "n_end"),
            ],
        }
    }

    fn ok_expression() -> Value {
        json!({
            "version": 1,
            "rules": [{
                "id": "r_big",
                "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10000},
                "then": [{"type": "ROLE", "role_codes": ["FINANCE_DIRECTOR"]}]
            }]
        })
    }

    fn r16(issues: &[Issue]) -> Vec<&Issue> {
        issues.iter().filter(|issue| issue.rule == "R16").collect()
    }

    #[test]
    fn assignee_kind_expression_is_parsed_but_not_enumerable() {
        assert_eq!(
            AssigneeKind::parse("expression"),
            Some(AssigneeKind::Expression),
            "保存时 AssigneeKind::parse(\"EXPRESSION\") 必须认得（否则用户选了表达式就报「非法」）"
        );
        assert_eq!(AssigneeKind::Expression.as_str(), "EXPRESSION");
        assert!(
            !AssigneeKind::Expression.is_enumerable(),
            "EXPRESSION 不落 biz_flow_node_assignee 行 ⇒ R14 不许要求「选具体的审批人」"
        );
    }

    #[test]
    fn r16_expression_without_config_is_65016() {
        let graph = graph_with_expression(json!({}));
        let issues = graph.expression_issues();
        let found = r16(&issues);
        assert_eq!(found.len(), 1, "必须恰好报一条 R16：{issues:#?}");
        assert_eq!(found[0].code, error_code::FLOW_CONDITION_INVALID);
        assert_eq!(found[0].code, 65016);
        assert!(
            found[0].message.contains("n_approve") && found[0].message.contains("财务审批"),
            "文案必须点名节点（key + 名称）：{}",
            found[0].message
        );
        // R14 不该同时误报「审批人没选」
        assert!(
            !issues.iter().any(|issue| issue.rule == "R14"),
            "EXPRESSION 不可枚举，R14 不许误报：{issues:#?}"
        );
    }

    #[test]
    fn r16_invalid_expression_structure_is_65016_with_reason() {
        // 第 2 条规则缺 when ⇒ 解析失败，必须带上具体原因（不是一句「不合法」）
        let mut expression = ok_expression();
        expression["rules"].as_array_mut().unwrap().push(json!({
            "id": "r_broken",
            "then": [{"type": "INITIATOR"}]
        }));
        let graph = graph_with_expression(json!({"expression": expression}));
        let issues = graph.expression_issues();
        let found = r16(&issues);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, 65016);
        assert!(
            found[0].message.contains("r_broken"),
            "文案必须定位到具体规则：{}",
            found[0].message
        );
    }

    #[test]
    fn r16_prev_approver_is_65033_not_65016() {
        let expression = json!({
            "version": 1,
            "rules": [{
                "id": "r_prev",
                "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 1},
                "then": [{"type": "PREV_APPROVER"}]
            }]
        });
        let graph = graph_with_expression(json!({"expression": expression}));
        let issues = graph.expression_issues();
        let found = r16(&issues);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].code,
            error_code::FLOW_EXPRESSION_INVALID,
            "D-E8：PREV_APPROVER 必须在发布期以 65033 拒绝（不是 501、也不是 65016）"
        );
        assert_eq!(found[0].code, 65033);
        assert!(
            found[0].message.contains("PREV_APPROVER"),
            "{}",
            found[0].message
        );
    }

    #[test]
    fn r16_valid_expression_produces_no_issue() {
        let graph = graph_with_expression(json!({"expression": ok_expression()}));
        assert!(
            graph.expression_issues().is_empty(),
            "合法表达式不许报 R16：{:#?}",
            graph.expression_issues()
        );
        // 整图校验也必须干净（R14 不误报、其它规则不该被 R16 影响）
        let issues = graph.validate();
        assert!(
            issues.is_empty(),
            "START → EXPRESSION 审批 → END 是合法图，不该有任何校验问题：{issues:#?}"
        );
    }

    #[test]
    fn r16_only_checks_expression_nodes() {
        // 静态来源的节点即使 config 里塞了坏 expression 也不该被 R16 管
        let mut graph = graph_with_expression(json!({"expression": json!({"bad": true})}));
        graph.nodes[1].assignee_type = Some(AssigneeKind::Initiator);
        assert!(
            graph.expression_issues().is_empty(),
            "R16 只对 assignee_type=EXPRESSION 生效"
        );
    }

    #[test]
    fn prev_approver_scan_looks_only_at_then_and_otherwise() {
        // 正例：then 里出现
        assert!(expression_uses_prev_approver(&json!({
            "rules": [{"id": "r1", "then": [{"type": "PREV_APPROVER"}]}]
        })));
        // 正例：otherwise 里出现
        assert!(expression_uses_prev_approver(&json!({
            "rules": [{"id": "r1", "then": [{"type": "ADMIN"}]}],
            "otherwise": [{"type": "prev_approver"}]
        })));
        // 反例：只在 when 的字段名里出现同名字符串，不是 target
        assert!(!expression_uses_prev_approver(&json!({
            "rules": [{
                "id": "r1",
                "when": {"op": "cmp", "field": "form.prev_approver", "cmp": "eq", "value": "PREV_APPROVER"},
                "then": [{"type": "ADMIN"}]
            }]
        })));
        // 反例：普通表达式
        assert!(!expression_uses_prev_approver(&ok_expression()));
    }

    #[test]
    fn validate_reports_r16_through_graph_validate() {
        // 端点 `/version/{vid}/validate` 与 publish 都只调 `graph.validate()`
        // ⇒ R16 必须从 validate() 冒出来（不是只挂在单独的 helper 上）
        let graph = graph_with_expression(json!({}));
        let issues = graph.validate();
        assert_eq!(r16(&issues).len(), 1, "{issues:#?}");
    }
}

// ==================== P-FLOW-1A：TASK_NODE / NOTIFY_NODE 的发布校验（R17–R21） ====================
//
// 每条规则**正例 + 反例**各一条（验收 3）。这些用例全部是纯逻辑（不连库），
// 因此可以在 `cargo test --lib business::flow` 里跑，不需要 SYS_DB_URL。

#[cfg(test)]
mod task_node_rule_tests {
    use super::*;
    use serde_json::json;

    /// 一个合法的 `config.task`（最小可用形状）。
    fn task_config() -> Value {
        json!({
            "title_template": "{{instance.biz_no}} 催货",
            "on_complete": "DIRECT",
            "due": {"mode": "RELATIVE", "amount": 3, "unit": "WORKDAY"}
        })
    }

    /// `START → TASK_NODE → END`（R17 正例的骨架）。
    fn task_graph(config: Value) -> FlowGraph {
        let mut task = NodeSpec::new("n_task", "催货", NodeType::Task);
        task.assignee_type = Some(AssigneeKind::Role);
        task.assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: "11111111-1111-1111-1111-111111111111".to_string(),
            sort_order: 1,
        }];
        // R20 的反例要 `SKIP`，这里默认给 ERROR（正确配置）
        task.empty_assignee = "ERROR".to_string();
        task.timeout_action = "REMIND".to_string();
        task.config = json!({ "task": config });
        FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                task,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_task"),
                EdgeSpec::new("e2", "n_task", "n_end"),
            ],
        }
    }

    fn rules(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|issue| issue.rule).collect()
    }

    /// `NodeType::parse` / `as_str` 往返（验收 3 第一条）。
    #[test]
    fn node_type_round_trip_covers_nine_values() {
        for node_type in NodeType::ALL {
            assert_eq!(
                NodeType::parse(node_type.as_str()),
                Some(node_type),
                "{} 必须能往返",
                node_type.as_str()
            );
            assert_eq!(
                NodeType::parse(&node_type.as_str().to_ascii_lowercase()),
                Some(node_type),
                "{} 的小写也必须能解析",
                node_type.as_str()
            );
        }
        assert_eq!(NodeType::parse("TASK_NODE"), Some(NodeType::Task));
        assert_eq!(NodeType::parse("NOTIFY_NODE"), Some(NodeType::Notify));
        assert_eq!(
            NodeType::parse("TASK"),
            None,
            "别名不认（库里只存 TASK_NODE）"
        );
        // 出度约束的分类：审批类不含任务/通知，但都属「出度必须 = 1」
        assert!(!NodeType::Task.is_approval_class());
        assert!(NodeType::Task.is_single_exit());
        assert!(NodeType::Notify.is_single_exit());
        // 指派来源：任务节点必须有负责人
        assert!(NodeType::Task.needs_assignee());
        assert!(!NodeType::Notify.needs_assignee());
    }

    /// R17 正例：`TASK_NODE` 出度 = 1 → 不报 R17。
    #[test]
    fn r17_task_node_single_exit_passes() {
        let issues = task_graph(task_config()).validate();
        assert!(
            !rules(&issues).contains(&"R17"),
            "出度为 1 不该报 R17：{issues:#?}"
        );
    }

    /// R17 反例：`TASK_NODE` 出度 = 2 → `65012`。
    #[test]
    fn r17_task_node_two_exits_rejected() {
        let mut graph = task_graph(task_config());
        graph.edges.push(EdgeSpec::new("e3", "n_task", "n_end"));
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R17")
            .expect("必须报 R17");
        assert_eq!(hit.code, error_code::FLOW_GRAPH_DEGREE);
        assert_eq!(hit.nodes, vec!["n_task".to_string()]);
    }

    /// R18 正例：合法的 `config.task` 通过 R17–R21 全部规则。
    #[test]
    fn r18_valid_task_config_passes_all_rules() {
        let issues = task_graph(task_config()).validate_with(Some("PURCHASE_ORDER"));
        let task_rules: Vec<&str> = rules(&issues)
            .into_iter()
            .filter(|rule| matches!(*rule, "R17" | "R18" | "R19" | "R20" | "R21"))
            .collect();
        assert!(task_rules.is_empty(), "合法配置不该报 R17–R21：{issues:#?}");
    }

    /// R18 反例：`TASK_NODE` 没有 `config.task` → `65034`。
    #[test]
    fn r18_task_node_without_config_rejected() {
        let mut graph = task_graph(task_config());
        graph.nodes[1].config = json!({});
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R18")
            .expect("必须报 R18");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("config.task"), "{}", hit.message);
    }

    /// R18 反例：`config.task` 配在 `APPROVAL` 上 → `65034`（分区纪律，不静默忽略）。
    #[test]
    fn r18_task_config_on_approval_rejected() {
        let mut graph = task_graph(task_config());
        graph.nodes[1].node_type = NodeType::Approval;
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R18")
            .expect("必须报 R18");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("TASK_NODE"), "{}", hit.message);
    }

    /// R18 正例（P-FLOW-1B）：`on_complete=APPROVE` + `need_review=false` **必须通过**。
    ///
    /// 1A 这里是一条「阶段闸」反例（拒绝并点名 P-FLOW-1B）。1B 把引擎分支做出来了
    /// （任务 `DOING→DONE` → 节点保持 ACTIVE + 同事务发起 `approve_biz_type` 审批单），
    /// 闸随之删除 —— 这条用例就是那个删除动作的回归证据。
    #[test]
    fn r18_approve_without_review_passes_in_1b() {
        let mut graph = task_graph(json!({
            "title_template": "t",
            "on_complete": "APPROVE",
            "approve_biz_type": "TASK_ACCEPT"
        }));
        graph.nodes[1].assignee_type = Some(AssigneeKind::Role);
        graph.nodes[1].assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: "11111111-1111-1111-1111-111111111111".to_string(),
            sort_order: 1,
        }];
        let issues = graph.validate();
        assert!(
            !issues.iter().any(|issue| issue.rule == "R18"),
            "APPROVE × need_review=false 不该再有 R18 问题：{:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    /// R18 反例（保留）：`on_complete=APPROVE` + `need_review=true` → `65035`（点两次）。
    #[test]
    fn r18_approve_with_review_still_rejected() {
        let mut graph = task_graph(json!({
            "title_template": "t",
            "need_review": true,
            "on_complete": "APPROVE",
            "approve_biz_type": "TASK_ACCEPT"
        }));
        graph.nodes[1].assignee_type = Some(AssigneeKind::Role);
        graph.nodes[1].assignees = vec![AssigneeRef {
            kind: "ROLE".to_string(),
            ref_id: "11111111-1111-1111-1111-111111111111".to_string(),
            sort_order: 1,
        }];
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R18" && issue.code == error_code::FLOW_TASK_NODE_INVALID)
            .expect("APPROVE × need_review=true 必须报 65035");
        assert!(
            hit.message.contains("点两次") || hit.message.contains("need_review"),
            "{}",
            hit.message
        );
    }

    /// 一个合法的 `config.notify`（最小可用形状：通知发起人 + 内联模板）。
    fn notify_config() -> Value {
        json!({
            "when": "ON_ENTER",
            "targets": [{"type": "INITIATOR"}],
            "inline": {"title": "{{instance.title}} 已到「{{node.name}}」", "content": "单号 {{instance.biz_no}}"}
        })
    }

    /// `START → NOTIFY_NODE → END`（**通知节点直接跟在 START 后面**：这是验收用例 ① 的图）。
    fn notify_graph(config: Value) -> FlowGraph {
        let mut notify = NodeSpec::new("n_notify", "通知", NodeType::Notify);
        notify.config = json!({ "notify": config });
        FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                notify,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_notify"),
                EdgeSpec::new("e2", "n_notify", "n_end"),
            ],
        }
    }

    /// R17 反例（NOTIFY）：通知节点出度 = 2 → `65012`（发布校验拒绝）。
    #[test]
    fn r17_notify_node_two_exits_rejected() {
        let mut graph = notify_graph(notify_config());
        graph.edges.push(EdgeSpec::new("e3", "n_notify", "n_end"));
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R17")
            .expect("必须报 R17");
        assert_eq!(hit.code, error_code::FLOW_GRAPH_DEGREE);
        assert_eq!(hit.nodes, vec!["n_notify".to_string()]);
        assert!(hit.message.contains("通知节点"), "{}", hit.message);
    }

    /// R18 反例（互斥）：`NOTIFY_NODE` 上配了 `config.task` → `400 + 65034`。
    ///
    /// 库侧还有 `ck_biz_flow_node_config_partition` 兜底，但那条抛出来是 500；
    /// 发布校验必须先在应用层给一句中文。
    #[test]
    fn r18_task_config_on_notify_rejected() {
        let mut graph = notify_graph(notify_config());
        graph.nodes[1].config = json!({
            "notify": notify_config(),
            "task": {"title_template": "t", "on_complete": "DIRECT"}
        });
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R18")
            .expect("必须报 R18");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("任务配置"), "{}", hit.message);
    }

    /// R19 正例（P-FLOW-2 放开）：合法的 `config.notify` **不再**被拒。
    ///
    /// 1A 时这条位置上是「R19 一律拒绝」的阶段闸用例；P-FLOW-2 把闸换成真正的配置校验，
    /// 所以这条用例现在证明的是「闸门已拆除，且合法配置一条 R19 都不报」。
    #[test]
    fn r19_notify_node_accepted_after_pflow2() {
        let issues = notify_graph(notify_config()).validate();
        let r19: Vec<&Issue> = issues.iter().filter(|issue| issue.rule == "R19").collect();
        assert!(r19.is_empty(), "合法通知配置不该报 R19：{r19:#?}");
        // 顺带钉住「NOTIFY 可以直接跟在 START 后面」这条设计决定
        assert!(
            !issues
                .iter()
                .any(|issue| issue.rule == "R1" || issue.rule == "R2"),
            "START → NOTIFY → END 必须是合法结构：{issues:#?}"
        );
    }

    /// R19 反例：没有 `config.notify` → `65034`。
    #[test]
    fn r19_notify_without_config_rejected() {
        let mut graph = notify_graph(notify_config());
        graph.nodes[1].config = json!({});
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R19")
            .expect("必须报 R19");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("config.notify"), "{}", hit.message);
    }

    /// R19 反例：内联模板用了白名单外的变量 → `65036`。
    #[test]
    fn r19_notify_inline_unknown_variable_rejected() {
        let mut graph = notify_graph(notify_config());
        graph.nodes[1].config = json!({
            "notify": {
                "when": "ON_ENTER",
                "targets": [{"type": "INITIATOR"}],
                "inline": {"title": "{{secret.password}}", "content": "x"}
            }
        });
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R19")
            .expect("必须报 R19");
        assert_eq!(hit.code, error_code::FLOW_NOTIFY_TEMPLATE_INVALID);
        assert!(
            hit.message.contains("secret.password"),
            "文案要点名错变量：{}",
            hit.message
        );
    }

    /// R19 反例：内联模板语法错（占位符未闭合）→ `65036`（**不是**运行期才 422）。
    #[test]
    fn r19_notify_inline_syntax_error_rejected() {
        let mut graph = notify_graph(notify_config());
        graph.nodes[1].config = json!({
            "notify": {
                "when": "ON_ENTER",
                "targets": [{"type": "INITIATOR"}],
                "inline": {"title": "{{instance.title", "content": "x"}
            }
        });
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R19")
            .expect("必须报 R19");
        assert_eq!(hit.code, error_code::FLOW_NOTIFY_TEMPLATE_INVALID);
        assert!(hit.message.contains("模板语法错误"), "{}", hit.message);
    }

    /// R19 反例：`when=ON_TIMEOUT` 但没配 `timeout_hours` → `65031`（永远等不到发送时刻）。
    #[test]
    fn r19_notify_on_timeout_requires_timeout_hours() {
        let mut graph = notify_graph(json!({
            "when": "ON_TIMEOUT",
            "targets": [{"type": "INITIATOR"}],
            "template_code": "FLOW_TODO"
        }));
        graph.nodes[1].timeout_hours = None;
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| {
                issue.rule == "R19" && issue.code == error_code::FLOW_TIMEOUT_CONFIG_INVALID
            })
            .expect("ON_TIMEOUT 缺 timeout_hours 必须报 65031");
        assert!(hit.message.contains("timeout_hours"), "{}", hit.message);
        // 配上就放行
        graph.nodes[1].timeout_hours = Some(4);
        let issues = graph.validate();
        assert!(
            !issues.iter().any(|issue| issue.rule == "R19"),
            "配上 timeout_hours 后不该再报 R19：{issues:#?}"
        );
    }

    /// R19 反例：`targets` 空 / 超 5 项 → `65034`（与 V0178 的 `BETWEEN 1 AND 5` 同口径）。
    #[test]
    fn r19_notify_targets_count_bounds() {
        let mut empty = notify_graph(notify_config());
        empty.nodes[1].config = json!({
            "notify": {"when": "ON_ENTER", "targets": [], "template_code": "FLOW_TODO"}
        });
        let hit = empty
            .validate()
            .into_iter()
            .find(|issue| issue.rule == "R19")
            .expect("空的 targets 必须报 R19");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("至少"), "{}", hit.message);

        let six: Vec<Value> = (0..6).map(|_| json!({"type": "INITIATOR"})).collect();
        let mut too_many = notify_graph(notify_config());
        too_many.nodes[1].config = json!({
            "notify": {"when": "ON_ENTER", "targets": six, "template_code": "FLOW_TODO"}
        });
        let hit = too_many
            .validate()
            .into_iter()
            .find(|issue| issue.rule == "R19")
            .expect("6 个 targets 必须报 R19");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("超过上限"), "{}", hit.message);
    }

    /// R20 反例：`empty_assignee=SKIP` 且没开 `allow_skip_when_no_assignee` → `65018`。
    #[test]
    fn r20_skip_requires_explicit_flag() {
        let mut graph = task_graph(task_config());
        graph.nodes[1].empty_assignee = "SKIP".to_string();
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R20" && issue.code == error_code::FLOW_NO_ASSIGNEE)
            .expect("必须报 65018");
        assert!(
            hit.message.contains("allow_skip_when_no_assignee"),
            "{}",
            hit.message
        );
    }

    /// R20 正例：开了 `allow_skip_when_no_assignee=true` 就放行（人确认过两次）。
    #[test]
    fn r20_skip_allowed_with_explicit_flag() {
        let mut graph = task_graph(json!({
            "title_template": "t",
            "on_complete": "DIRECT",
            "allow_skip_when_no_assignee": true
        }));
        graph.nodes[1].empty_assignee = "SKIP".to_string();
        let issues = graph.validate();
        assert!(
            !issues
                .iter()
                .any(|issue| issue.rule == "R20" && issue.code == error_code::FLOW_NO_ASSIGNEE),
            "开关打开后不该再报 65018：{issues:#?}"
        );
    }

    /// R20 反例：`timeout_action=AUTO_APPROVE` → `65031`（活没干完不能算干完）。
    #[test]
    fn r20_timeout_action_restricted() {
        let mut graph = task_graph(task_config());
        graph.nodes[1].timeout_action = "AUTO_APPROVE".to_string();
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.code == error_code::FLOW_TIMEOUT_CONFIG_INVALID)
            .expect("必须报 65031");
        assert_eq!(hit.rule, "R20");
        // ESCALATE 是允许的
        graph.nodes[1].timeout_action = "ESCALATE".to_string();
        let after = graph.validate();
        assert!(
            !after
                .iter()
                .any(|issue| issue.code == error_code::FLOW_TIMEOUT_CONFIG_INVALID),
            "ESCALATE 必须放行：{after:#?}"
        );
    }

    /// R20 反例：任务节点没配指派来源 → `65015`（规则编号是 R20，不是 R14）。
    #[test]
    fn r20_task_node_requires_assignee_source() {
        let mut graph = task_graph(task_config());
        graph.nodes[1].assignee_type = None;
        graph.nodes[1].assignees.clear();
        let issues = graph.validate();
        let hit = issues
            .iter()
            .find(|issue| issue.code == error_code::FLOW_NODE_ASSIGNEE_MISSING)
            .expect("必须报 65015");
        assert_eq!(hit.rule, "R20", "任务节点缺负责人归 R20，不能记到 R14 名下");
        assert!(hit.message.contains("负责人"), "{}", hit.message);
    }

    /// R21 反例 ①：变量路径不在白名单 → `65037`。
    #[test]
    fn r21_unknown_variable_path_rejected() {
        let graph = task_graph(json!({
            "title_template": "{{order.amount}} 催货",
            "on_complete": "DIRECT"
        }));
        let issues = graph.validate_with(Some("PURCHASE_ORDER"));
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R21")
            .expect("必须报 R21");
        assert_eq!(hit.code, error_code::FLOW_TEMPLATE_VAR_INVALID);
        assert!(hit.message.contains("order.amount"), "{}", hit.message);
    }

    /// R21 正例 ②：`task.*` 在 `TASK_ACCEPT` 流程里可用；反例：别的流程拒绝。
    #[test]
    fn r21_task_vars_only_on_task_accept() {
        let graph = task_graph(json!({
            "title_template": "{{task.title}} 验收",
            "on_complete": "DIRECT"
        }));
        let ok = graph.validate_with(Some("TASK_ACCEPT"));
        assert!(
            !ok.iter().any(|issue| issue.rule == "R21"),
            "TASK_ACCEPT 流程里 task.* 必须放行：{ok:#?}"
        );
        let bad = graph.validate_with(Some("PURCHASE_ORDER"));
        let hit = bad
            .iter()
            .find(|issue| issue.rule == "R21")
            .expect("非 TASK_ACCEPT 必须报 R21");
        assert_eq!(hit.code, error_code::FLOW_TEMPLATE_VAR_INVALID);
        // `biz_type = None`（编辑器「只校验不保存」的旧调用点）同样不放行
        let unknown = graph.validate_with(None);
        assert!(
            unknown.iter().any(|issue| issue.rule == "R21"),
            "未知 biz_type 时 task.* 也必须拒绝：{unknown:#?}"
        );
    }

    /// R21 反例 ③：`due.mode=FIELD` 的字段不是 `form.*` → `65034`。
    #[test]
    fn r21_due_field_must_be_form_path() {
        let graph = task_graph(json!({
            "title_template": "t",
            "on_complete": "DIRECT",
            "due": {"mode": "FIELD", "field": "instance.title"}
        }));
        let issues = graph.validate_with(Some("PURCHASE_ORDER"));
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R21")
            .expect("必须报 R21");
        assert_eq!(hit.code, error_code::FLOW_NODE_CONFIG_MISMATCH);
        assert!(hit.message.contains("form.*"), "{}", hit.message);
    }

    /// R21 反例 ④：模板超过一层点号 → `65036`（语法错，不是白名单错）。
    #[test]
    fn r21_nested_dot_variable_is_syntax_error() {
        let graph = task_graph(json!({
            "title_template": "{{form.a.b}} 催货",
            "on_complete": "DIRECT"
        }));
        let issues = graph.validate_with(Some("PURCHASE_ORDER"));
        let hit = issues
            .iter()
            .find(|issue| issue.rule == "R21")
            .expect("必须报 R21");
        assert_eq!(hit.code, error_code::FLOW_NOTIFY_TEMPLATE_INVALID);
    }

    // ---------- R22：节点兜底人（P3-03） ----------

    /// 造一张「START → 审批 → END」的最小图，审批节点由 `tweak` 改。
    ///
    /// 不复用别的测试模块的 `minimal()`（那是兄弟模块的私有函数）：
    /// 这个骨架只需要「一个审批类节点 + 合法出度」，自建最省事也最不受别的模块改动影响。
    fn approval_graph(tweak: impl FnOnce(&mut NodeSpec)) -> FlowGraph {
        let mut approve = NodeSpec::new("n_approve", "主管审批", NodeType::Approval);
        approve.assignee_type = Some(AssigneeKind::InitiatorSuperior);
        tweak(&mut approve);
        FlowGraph {
            nodes: vec![
                NodeSpec::new("n_start", "开始", NodeType::Start),
                approve,
                NodeSpec::new("n_end", "结束", NodeType::End),
            ],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_approve"),
                EdgeSpec::new("e2", "n_approve", "n_end"),
            ],
        }
    }

    fn r22(issues: &[Issue]) -> Vec<&Issue> {
        issues.iter().filter(|issue| issue.rule == "R22").collect()
    }

    /// R22 正例：只配兜底人 / 只配兜底角色 / 都不配 —— 都不该报。
    #[test]
    fn r22_accepts_exclusive_fallback_targets() {
        let user = Uuid::from_u128(11);
        let role = Uuid::from_u128(12);

        let none = approval_graph(|_| {}).validate_with(None);
        assert!(r22(&none).is_empty(), "不配兜底不该报 R22");

        let by_user = approval_graph(|node| node.fallback_user_id = Some(user)).validate_with(None);
        assert!(
            r22(&by_user).is_empty(),
            "只配兜底人应通过：{:?}",
            r22(&by_user).iter().map(|i| &i.message).collect::<Vec<_>>()
        );

        let by_role = approval_graph(|node| node.fallback_role_id = Some(role)).validate_with(None);
        assert!(r22(&by_role).is_empty(), "只配兜底角色应通过");
    }

    /// R22 反例 ①：两个都配 → `65042`（「到底听谁」的歧义；DB CHECK 也会挡，但库错误没人看得懂）。
    #[test]
    fn r22_rejects_both_targets() {
        let graph = approval_graph(|node| {
            node.fallback_user_id = Some(Uuid::from_u128(11));
            node.fallback_role_id = Some(Uuid::from_u128(12));
        });
        let issues = graph.validate_with(None);
        let hits = r22(&issues);
        assert_eq!(hits.len(), 1, "只报一条");
        assert_eq!(hits[0].code, error_code::FLOW_FALLBACK_INVALID);
        assert_eq!(hits[0].code, 65042);
        assert!(
            hits[0].message.contains("只能二选一"),
            "{}",
            hits[0].message
        );
    }

    /// R22 反例 ②：兜底人 == 节点 `assignee_type=USER` 的指定人 → `65042`（无意义配置）。
    #[test]
    fn r22_rejects_fallback_equal_to_designated_user() {
        let same = Uuid::from_u128(21);
        let graph = approval_graph(|node| {
            node.assignee_type = Some(AssigneeKind::User);
            node.assignees = vec![AssigneeRef {
                kind: "USER".to_string(),
                ref_id: same.to_string(),
                sort_order: 0,
            }];
            node.fallback_user_id = Some(same);
        });
        let issues = graph.validate_with(None);
        let hits = r22(&issues);
        assert_eq!(hits.len(), 1, "{:?}", hits);
        assert_eq!(hits[0].code, 65042);
        assert!(
            hits[0].message.contains("没有兜底"),
            "报文要说清「等于没兜底」：{}",
            hits[0].message
        );

        // 换一个人就不该报（兜底的意义就是「主路不行时换他」）
        let graph = approval_graph(|node| {
            node.assignee_type = Some(AssigneeKind::User);
            node.assignees = vec![AssigneeRef {
                kind: "USER".to_string(),
                ref_id: same.to_string(),
                sort_order: 0,
            }];
            node.fallback_user_id = Some(Uuid::from_u128(22));
        });
        assert!(r22(&graph.validate_with(None)).is_empty());
    }

    /// R22 反例 ③：兜底配置放在没有审批语义的节点上 → `65042`（静默忽略比报错更坏）。
    #[test]
    fn r22_rejects_fallback_on_non_approval_node() {
        let graph = approval_graph(|node| {
            node.node_type = NodeType::Condition;
            node.assignee_type = None;
            node.fallback_role_id = Some(Uuid::from_u128(31));
        });
        // 条件节点本身还有别的规则会红（R7 出度 ≥ 2 / R13 兜底分支），只挑 R22 断言
        let issues = graph.validate_with(None);
        let hits = r22(&issues);
        assert_eq!(hits.len(), 1, "{:?}", hits);
        assert_eq!(hits[0].code, 65042);
        assert!(
            hits[0].message.contains("不允许配置兜底审批人"),
            "{}",
            hits[0].message
        );
    }

    /// 快照里未配兜底时不出现这两个键 —— 存量已发布流程的 `graph_hash` 一个字节都不能变
    /// （否则一次「纯加字段」的升级会把所有版本的 hash 改掉，版本比对全部误报）。
    #[test]
    fn snapshot_omits_fallback_when_unset() {
        let graph = approval_graph(|_| {});
        let text = graph.snapshot_json();
        assert!(
            !text.contains("fallback_user_id"),
            "未配兜底时不该出现在快照里"
        );
        assert!(!text.contains("fallback_role_id"));

        let mut with_fallback = graph.clone();
        let approve = with_fallback
            .nodes
            .iter_mut()
            .find(|node| node.key == "n_approve")
            .expect("审批节点");
        approve.fallback_role_id = Some(Uuid::from_u128(77));
        let text = with_fallback.snapshot_json();
        assert!(text.contains("fallback_role_id"));
        assert!(text.contains(&Uuid::from_u128(77).to_string()));
        // 配了与没配必须是不同的 hash（否则「改了配置却不重新发布」会静默生效）
        assert_ne!(with_fallback.graph_hash(), graph.graph_hash());
    }
}
