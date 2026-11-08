//! 审批流 · `65018`「无可用审批人」的**结构化诊断**（FIX-1 新环境开箱即用）。
//!
//! # 为什么要有这个模块
//!
//! `65018` 的老报文是一句话：「本节点没有一个可用审批人（empty_assignee=ERROR）」。
//! 对**老环境**够用（管理员知道去哪儿改），但对**刚刚部署的空库**是死路：
//! 新环境只有 `admin` 一个账号，制单人就是 admin，
//!
//! ```text
//! admin 建销售订单 → 提交 → 节点「销售主管审批」绑 SALES_SUPERVISOR（0 人）
//!   → 兜底超管 = admin = 制单人 → 内控（职责分离）剔掉 → 0 人 → 422 / 65018
//! ```
//!
//! 内控本身是对的（fail-closed，不许自审批），但前端只能弹一句红字，用户不知道
//! 「缺哪个角色、那个角色现在几个人、下一步点哪里」。于是本模块把失败原因翻成
//! **机读载荷**，跟着错误体一起回给前端：
//!
//! ```json
//! {
//!   "node_key": "n_sales", "node_name": "销售主管审批",
//!   "assignee_type": "ROLE", "role_code": "SALES_SUPERVISOR", "role_name": "销售主管",
//!   "member_count": 0, "reason": "ROLE_NO_MEMBER",
//!   "fix_hint": "请为角色「销售主管」指派至少一名在职成员，或为该节点指定固定审批人"
//! }
//! ```
//!
//! # 分层：**先分类，再查库**
//!
//! [`classify`] 是**纯函数**（不查库、不 `await`），输入是「解析过程中已经算出来的
//! 计数」（候选人数 / 启用人数 / 过 ROBOT 后人数 / 过自审后人数）+「角色是否存在」，
//! 输出一个 [`Reason`]。这样做有两个好处：
//!
//! 1. 每一层原因都有单测钉住（`ROLE_NO_MEMBER` 与 `ROLE_MEMBER_UNAVAILABLE` 是
//!    **两种完全不同的处置**：前者去加人，后者先查「人是不是停用了」）；
//! 2. 运行期只需要一次 `sys_role` 查询，不会为了「诊断」把审批人解析再跑一遍
//!    （那会让「解析出 0 人」变成「解析出 0 人 + 又解析一次」，两份结论迟早漂移）。
//!
//! # 覆盖范围：所有审批流共用
//!
//! 本模块**不认业务类型**（销售 / 采购 / 报销 / 预算 / 盘点 / 处置 / 付款全一样），
//! 调用点是流程引擎自己：
//!
//! | 调用点 | 触发情形 |
//! |--------|----------|
//! | `assignee::resolve` | `empty_assignee=ERROR` 解析出 0 人 / 兜底超管也不存在 |
//! | `engine`（审批节点） | 解析出来的人**全被内控（制单人 / 发起人）剔掉** → 0 人 |
//!
//! 因此「一劳永逸」不是口号：新增业务类型只要走引擎，就自动拿到同一份诊断。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::business::flow::assignee::{self, Candidate};
use crate::business::flow::graph::{AssigneeKind, SnapshotNode};
use crate::shared::error_code;

// ==================== 信号（解析期已经算出来的计数） ====================

/// 解析链路上各层的「还剩几个人」。
///
/// 顺序对应 `assignee::finalize` 的四个过滤步骤（① 停用 ② ROBOT ③ 去重 ④ 自审），
/// 计数口径逐字对齐那个函数，**不在这里重算策略**。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Signal {
    /// 节点配置里该来源的目标个数（角色数 / 用户数 / 部门数；INITIATOR 类为 1）。
    pub targets: usize,
    /// 解析出来的候选人数（去重前、过滤前）。
    pub candidates: usize,
    /// 其中启用状态的人数（`finalize` ① 之后）。
    pub alive: usize,
    /// 过 ROBOT 过滤后的人数（`finalize` ② 之后）。
    pub eligible: usize,
    /// 过自审策略后的人数（`finalize` ④ 之后；>0 就说明有人可签）。
    pub non_self: usize,
}

impl Signal {
    /// 从「候选 + 策略参数」推出各层计数（**与 `finalize` 同一口径**，不重新实现策略）。
    ///
    /// 只用于「解析结果为空、需要解释」的路径，因此不必精确到 SKIPPED 留痕行数。
    pub fn of(spec: &SnapshotNode, candidates: &[Candidate], initiator: Uuid) -> Self {
        let alive = candidates.iter().filter(|c| c.active).count();
        let eligible = candidates
            .iter()
            .filter(|c| c.active)
            .filter(|c| spec.allow_robot || !c.is_robot || c.source == AssigneeKind::User)
            .count();
        let non_self = candidates
            .iter()
            .filter(|c| c.active)
            .filter(|c| spec.allow_robot || !c.is_robot || c.source == AssigneeKind::User)
            .filter(|c| c.user_id != initiator)
            .count();
        Self {
            targets: configured_targets(spec),
            candidates: candidates.len(),
            alive,
            eligible,
            non_self,
        }
    }

    /// 「内控把审批人全剔掉」专用：进来的人都是**已经通过** `finalize` 的可用候选，
    /// 只是全部是制单人 / 发起人。因此三层计数都等于候选数，`non_self = 0`。
    pub fn all_excluded(count: usize) -> Self {
        Self {
            targets: 0,
            candidates: count,
            alive: count,
            eligible: count,
            non_self: 0,
        }
    }
}

/// 节点配置里该来源配了几个目标（角色 / 部门 / 用户）。
fn configured_targets(spec: &SnapshotNode) -> usize {
    match spec
        .assignee_type
        .as_deref()
        .map(|raw| AssigneeKind::parse(raw.trim()))
    {
        Some(Some(AssigneeKind::Role)) => assignee::role_ids_of(spec).len(),
        Some(Some(AssigneeKind::DeptLeader)) => assignee::dept_ids_of(spec).len(),
        Some(Some(AssigneeKind::User)) => assignee::user_ids_of(spec).len(),
        // 运行时才算得出来的来源（发起人 / 主管 / 任务方 / 表单变量）：配置就是 1 个「意图」
        Some(Some(_)) => 1,
        _ => 0,
    }
}

// ==================== 原因分类（纯函数） ====================

/// 「为什么一个人都没有」。
///
/// # 为什么分这么细
///
/// 每一档的**下一步动作**不同，混成一句「没有可用审批人」等于让用户猜：
///
/// | 原因 | 用户该做什么 |
/// |------|--------------|
/// | [`Reason::RoleNoMember`] | 去角色管理给这个角色加人（或一键建演示账号） |
/// | [`Reason::RoleMemberUnavailable`] | 加人之前先看：成员是不是被停用了 / 离职了 |
/// | [`Reason::RoleNotFound`] | 角色被删或改名了 → 重新配节点的审批人来源 |
/// | [`Reason::SelfOnly`] | 岗位有人，但**都是制单人自己** → 换个人提交或再加一名成员 |
/// | [`Reason::DeptNoLeader`] | 部门没设负责人 → 去组织架构里设 |
/// | [`Reason::NodeNotConfigured`] | 节点根本没配审批人 → 去流程设计器里配 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// 节点没配审批人来源（或写了个不认识的取值）。
    NodeNotConfigured,
    /// `ROLE` 来源里配置的角色一个都查不到（被软删 / 改名）。
    RoleNotFound,
    /// 角色存在，但**在位成员 0 行**。
    RoleNoMember,
    /// 角色有成员行，但解析出来 0 个可用候选（成员全被停用 / 软删 / 授权过期）。
    RoleMemberUnavailable,
    /// 有候选，但全是机器人且节点不允许机器人。
    RobotOnly,
    /// 有候选，但全部是发起人 / 制单人本人（自审 SKIP 或内控职责分离剔掉）。
    SelfOnly,
    /// `USER` 来源指定的人不可用（软删 / 停用）。
    UserUnavailable,
    /// `DEPT_LEADER` 来源的部门没设负责人。
    DeptNoLeader,
    /// `INITIATOR_SUPERIOR` 上溯不到部门负责人（没部门 / 层级不够 / 部门没负责人）。
    SuperiorNotFound,
    /// `TASK_OWNER` / `CREATOR` / `ASSIGNER` 关联任务缺失或对应人为空。
    TaskPartyMissing,
    /// `FORM_VAR` 表单变量没填或填错。
    FormVarMissing,
    /// 兜底：以上都不匹配（新增来源时不会静默变成空字符串）。
    NoCandidate,
}

impl Reason {
    /// 机读码（前端按它分支，**改码即破坏契约**）。
    pub fn code(self) -> &'static str {
        match self {
            Reason::NodeNotConfigured => "NODE_NOT_CONFIGURED",
            Reason::RoleNotFound => "ROLE_NOT_FOUND",
            Reason::RoleNoMember => "ROLE_NO_MEMBER",
            Reason::RoleMemberUnavailable => "ROLE_MEMBER_UNAVAILABLE",
            Reason::RobotOnly => "ROBOT_ONLY",
            Reason::SelfOnly => "SELF_ONLY",
            Reason::UserUnavailable => "USER_UNAVAILABLE",
            Reason::DeptNoLeader => "DEPT_NO_LEADER",
            Reason::SuperiorNotFound => "SUPERIOR_NOT_FOUND",
            Reason::TaskPartyMissing => "TASK_PARTY_MISSING",
            Reason::FormVarMissing => "FORM_VAR_MISSING",
            Reason::NoCandidate => "NO_CANDIDATE",
        }
    }

    /// 人读的一句话（前端可直接展示，不含内部路径）。
    pub fn text(self) -> &'static str {
        match self {
            Reason::NodeNotConfigured => "该节点没有配置审批人来源",
            Reason::RoleNotFound => "节点绑定的角色已被删除或改名",
            Reason::RoleNoMember => "该角色下没有任何在职成员",
            Reason::RoleMemberUnavailable => "该角色有成员记录，但成员均已停用或已删除",
            Reason::RobotOnly => "该节点只解析到机器人账号，而节点不允许机器人审批",
            Reason::SelfOnly => "该节点解析到的审批人都是制单人 / 发起人本人（内控不允许自审）",
            Reason::UserUnavailable => "节点指定的审批人已停用或已删除",
            Reason::DeptNoLeader => "该节点绑定的部门没有设置负责人",
            Reason::SuperiorNotFound => "发起人没有部门，或其上级部门没有设置负责人",
            Reason::TaskPartyMissing => "关联任务的负责人 / 创建人 / 分派人缺失",
            Reason::FormVarMissing => "表单里用于指定审批人的变量没有填写或填写有误",
            Reason::NoCandidate => "该节点没有任何可用审批人",
        }
    }

    /// 「下一步怎么办」——一句话，带**具体去哪**。
    pub fn hint(self, role_name: &str) -> String {
        match self {
            Reason::NodeNotConfigured => {
                "请到「流程定义 → 本流程 → 该节点」把审批人设为具体角色或人员".to_string()
            }
            Reason::RoleNotFound => {
                "节点绑定的角色已不存在：请到「流程定义 → 本流程 → 该节点」重新选择审批角色"
                    .to_string()
            }
            Reason::RoleNoMember => format!(
                "请为角色「{role_name}」指派至少一名在职成员（角色管理 → 成员），或为节点指定固定审批人；\
                 开发 / 测试环境也可一键创建演示账号"
            ),
            Reason::RoleMemberUnavailable => format!(
                "角色「{role_name}」的成员都已停用或已删除：请恢复 / 新增在职成员，或为该节点另选角色"
            ),
            Reason::RobotOnly => {
                "请给该节点配置真人审批人，或在节点上打开「允许机器人审批」".to_string()
            }
            Reason::SelfOnly => format!(
                "角色「{role_name}」解析出的审批人全部是制单人 / 发起人本人：请再指派一名**其他**成员，\
                 或改由他人提交本单"
            ),
            Reason::UserUnavailable => {
                "请到「流程定义 → 本流程 → 该节点」重新指定在职人员".to_string()
            }
            Reason::DeptNoLeader => {
                "请到「组织架构」给该部门设置负责人，或改由角色 / 指定人员审批".to_string()
            }
            Reason::SuperiorNotFound => {
                "请给发起人所在部门设置负责人（或上溯层级的部门负责人），或把节点改成其它审批人来源"
                    .to_string()
            }
            Reason::TaskPartyMissing => {
                "请检查关联任务是否存在、对应的人员字段是否已填写".to_string()
            }
            Reason::FormVarMissing => {
                "请在表单里选择审批人（该节点按表单变量取人），或改由角色审批".to_string()
            }
            Reason::NoCandidate => {
                "请检查该节点的审批人配置，并确认对应角色下有在职成员".to_string()
            }
        }
    }
}

/// 分类（**纯函数**，可单测）。
///
/// # 判据顺序
///
/// 1. 一个候选都没有 → 按来源逐档定位（角色不存在 / 角色没人 / 人不可用 / 部门没负责人…）；
/// 2. 有候选但全被停用 → [`Reason::RoleMemberUnavailable`]；
/// 3. 有候选但全被 ROBOT 过滤 → [`Reason::RobotOnly`]；
/// 4. 有候选但全是本人 → [`Reason::SelfOnly`]（**新环境最常见的坑**）；
/// 5. 以上都不是 → [`Reason::NoCandidate`]（不静默）。
///
/// `role_found` / `role_member_rows` 只对 `ROLE` 来源有意义，其它来源传
/// `false` / `0` 即可。
pub fn classify(kind: &str, role_found: bool, role_member_rows: i64, signal: Signal) -> Reason {
    let kind = kind.trim().to_ascii_uppercase();
    if signal.candidates == 0 {
        return match kind.as_str() {
            "ROLE" => {
                if signal.targets == 0 {
                    Reason::NodeNotConfigured
                } else if !role_found {
                    Reason::RoleNotFound
                } else if role_member_rows == 0 {
                    Reason::RoleNoMember
                } else {
                    Reason::RoleMemberUnavailable
                }
            }
            "USER" => Reason::UserUnavailable,
            "DEPT_LEADER" => Reason::DeptNoLeader,
            "INITIATOR_SUPERIOR" => Reason::SuperiorNotFound,
            "TASK_OWNER" | "TASK_CREATOR" | "TASK_ASSIGNER" => Reason::TaskPartyMissing,
            "FORM_VAR" => Reason::FormVarMissing,
            // INITIATOR 不可能 0 人（发起人本人就是候选）；未知 / 空来源 → 未配置
            "INITIATOR" | "ADMIN" => Reason::NoCandidate,
            _ => Reason::NodeNotConfigured,
        };
    }
    if signal.alive == 0 {
        return Reason::RoleMemberUnavailable;
    }
    if signal.eligible == 0 {
        return Reason::RobotOnly;
    }
    if signal.non_self == 0 {
        return Reason::SelfOnly;
    }
    Reason::NoCandidate
}

// ==================== 查询：角色的 code / name / 成员数 ====================

/// 一个角色在库里的样子（诊断用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleInfo {
    /// 角色 id。
    pub id: Uuid,
    /// 角色编码（如 `SALES_SUPERVISOR`）。
    pub role_code: String,
    /// 角色名称（如 `销售主管`）。
    pub role_name: String,
    /// 在位成员行数（未过期、用户未软删，**不论启停**）。
    pub member_rows: i64,
    /// 其中「启用且非机器人」的人数。
    pub active_humans: i64,
}

/// 按角色 id 查 code / name / 成员数（未软删的角色）。
///
/// 口径与 `assignee::load_role_members` 对齐：`expire_time IS NULL OR > now()`、
/// 用户 `deleted_at IS NULL`；并且额外统计「启用且非机器人」的人数，用于区分
/// 「角色没人」与「人都停用了」。
pub async fn role_infos(ex: &impl SqlExecutor, role_ids: &[Uuid]) -> DbResult<Vec<RoleInfo>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            "SELECT r.id AS role_id, r.role_code AS role_code, r.role_name AS role_name, \
                    count(ur.id) AS member_rows, \
                    count(ur.id) FILTER (WHERE u.status = 1 AND u.user_type <> 'ROBOT') AS active_humans \
               FROM sys_role r \
               LEFT JOIN sys_user_role ur ON ur.role_id = r.id \
                    AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
               LEFT JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
              WHERE r.id = ANY(?::text::uuid[]) AND r.deleted_at IS NULL \
              GROUP BY r.id, r.role_code, r.role_name \
              ORDER BY r.role_code"
                .to_string(),
            vec![assignee::array_param(role_ids)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(RoleInfo {
            id: row.get_typed("role_id")?,
            role_code: row.get_typed("role_code")?,
            role_name: row.get_typed("role_name")?,
            member_rows: row.get_typed("member_rows")?,
            active_humans: row.get_typed("active_humans")?,
        });
    }
    Ok(out)
}

// ==================== 载荷组装 ====================

/// 诊断载荷（序列化成错误体的 `data`）。
///
/// 字段名就是**前端契约**：`frontend/src/utils/flow-assignee-guard.ts` 按这些名字读，
/// 改名字要两边一起改（有单测钉住字段集合）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoAssigneePayload {
    /// 流程实例 id（发起失败时可能是「将要创建但已回滚」的 id，仅供排障关联）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_instance_id: Option<String>,
    /// 实例节点行 id（`biz_flow_instance_node.id`；解析期还没落库时为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// 节点 key（如 `n_sales`）。
    pub node_key: String,
    /// 节点名称（如「销售主管审批」）。
    pub node_name: String,
    /// 审批人来源（`ROLE` / `USER` / `DEPT_LEADER` / `INITIATOR_SUPERIOR` …）。
    pub assignee_type: String,
    /// 角色 id（来源是 ROLE 且角色存在时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    /// 角色编码（前端据此调「指派审批人」接口，跨环境稳定）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_code: Option<String>,
    /// 角色名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_name: Option<String>,
    /// 该角色当前**在职成员数**（0 = 角色没人，前端引导「加人」）。
    pub member_count: i64,
    /// 该角色「启用且非机器人」的人数（`member_count > 0 && usable_member_count == 0` ⇒ 人都停用了）。
    pub usable_member_count: i64,
    /// 解析出的候选人数（去重前）。
    pub candidate_count: usize,
    /// 机读原因码（见 [`Reason::code`]）。
    pub reason: String,
    /// 原因的人读说明。
    pub reason_text: String,
    /// 被排除的人（「张三：发起人本人」），便于用户知道「明明有人为什么不行」。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    /// 升级链走过的每一级（P3-02）：`role_code` / `role_name` / `member_count` / `usable_count`。
    ///
    /// 空 = 没走升级链（不是角色来源、或第一级就找到人了）。非空 = 「升级链已走完仍无人」，
    /// 前端据此把每一级的角色名与人数直接摊开，而不是让用户自己猜缺谁。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub escalation_chain: Vec<Json>,
    /// 升级原因（`SKIP_SELF_APPROVAL` / `NO_MEMBER`；没走升级链则缺省）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_reason: Option<String>,
    /// 原本该审的角色编码（升级链被触发时的起点）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalated_from_role: Option<String>,
    /// 下一步怎么办（带具体去处）。
    pub fix_hint: String,
}

impl NoAssigneePayload {
    /// 转成 JSON（错误体 `data`）。
    pub fn to_json(&self) -> Json {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

/// 组装诊断载荷。
///
/// # 参数
///
/// - `instance_id`：流程实例 id（`InstanceCtx::instance_id`）；
/// - `spec`：快照节点（拿节点 key / 名称 / 来源 / 角色 id）；
/// - `signal`：解析期计数（见 [`Signal`]）；
/// - `excluded`：被剔除的人与原因（内控 / 自审），前端展示「为什么有人也不行」；
/// - `escalation`：升级链走过的每一级（P3-02；`None` = 没走链）。到顶仍无人时它会被展开成
///   `escalation_chain[]`，前端就能直接回答「系统已经替你想过哪几级上级角色、各有几个人」。
///
/// 「当前登录人能不能指派」**不**放在载荷里：那是权限判断（`system:setup:manage`），
/// 权限在令牌里、不在库里——前端直接 `hasPermission('system:setup:manage')` 判断，
/// 后端由接口上的 `#[nik::permission]` 兜住（无权限 = 403，不是「按钮藏起来」）。
///
/// # 异常情况
///
/// 查库失败**不**让诊断反过来把流程搞挂：查不到角色信息时退化成「不带角色字段」的载荷
/// （`role_code = None`），`reason` 仍按 `ROLE_NOT_FOUND` 报出。
pub async fn payload(
    ex: &impl SqlExecutor,
    instance_id: Uuid,
    spec: &SnapshotNode,
    signal: Signal,
    excluded: Vec<String>,
    escalation: Option<&assignee::Escalation>,
) -> Json {
    let kind = spec
        .assignee_type
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    let role_ids = assignee::role_ids_of(spec);
    let infos = role_infos(ex, &role_ids).await.unwrap_or_default();
    // 一个节点可能配多个角色（「任一角色有人」即可）：诊断优先挑**没人**的那个角色作示例
    let pick = infos
        .iter()
        .find(|info| info.active_humans == 0)
        .or_else(|| infos.first());
    let role_found = !role_ids.is_empty() && !infos.is_empty();
    let role_member_rows: i64 = infos.iter().map(|info| info.member_rows).sum();

    let reason = classify(&kind, role_found, role_member_rows, signal);
    let role_name = pick.map(|info| info.role_name.clone()).unwrap_or_default();
    let mut payload = NoAssigneePayload {
        flow_instance_id: Some(instance_id.to_string()),
        node_id: None,
        node_key: spec.key.clone(),
        node_name: spec.name.clone(),
        assignee_type: if kind.is_empty() {
            "未配置".to_string()
        } else {
            kind.clone()
        },
        role_id: pick.map(|info| info.id.to_string()),
        role_code: pick.map(|info| info.role_code.clone()),
        role_name: pick.map(|info| info.role_name.clone()),
        member_count: pick.map(|info| info.member_rows).unwrap_or(0),
        usable_member_count: pick.map(|info| info.active_humans).unwrap_or(0),
        candidate_count: signal.candidates,
        reason: reason.code().to_string(),
        reason_text: reason.text().to_string(),
        excluded,
        escalation_chain: escalation
            .map(|escalation| escalation.to_payload_chain())
            .unwrap_or_default(),
        escalation_reason: escalation
            .filter(|escalation| !escalation.reason.is_empty())
            .map(|escalation| escalation.reason.clone()),
        escalated_from_role: escalation
            .map(|escalation| escalation.from_role_code.clone())
            .filter(|code| !code.is_empty()),
        fix_hint: reason.hint(&role_name),
    };
    // 「角色有成员行但一个都用不上」时，把「停用 / 删除」说清楚（前端据此提示「先看成员状态」）
    if reason == Reason::RoleMemberUnavailable {
        payload.reason_text = format!(
            "该角色有 {} 条成员记录，但没有一个可用（成员被停用 / 已删除 / 授权已过期）",
            payload.member_count
        );
    }
    payload.to_json()
}

/// 把载荷挂到 65018 错误上（**只挂一次**，重复调用不覆盖）。
///
/// 只在 `error.code == 65018` 时生效：别的错误拿到载荷说明调用方接错了线，
/// 静默忽略比挂上去更安全（避免"所有错误都带一个审批人载荷"这种脏数据）。
pub fn attach(error: &mut ApiError, payload: Json) {
    if error.code == error_code::FLOW_NO_ASSIGNEE && error.data.is_none() {
        error.data = Some(payload);
    }
}

/// 补齐实例节点行 id（`biz_flow_instance_node.id`）。
///
/// 解析期（`assignee::resolve`）拿不到行 id —— 那是引擎在落待办时才有的；引擎在
/// 报错前调用本函数补齐，前端就能直接定位到「哪一个实例节点」。
pub fn set_node_id(error: &mut ApiError, node_id: Uuid) {
    if error.code != error_code::FLOW_NO_ASSIGNEE {
        return;
    }
    if let Some(Json::Object(map)) = error.data.as_mut() {
        map.insert("node_id".to_string(), json!(node_id.to_string()));
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::graph::SnapshotNode;

    /// 造一个最小快照节点（只看审批人相关字段）。
    fn node(kind: &str, allow_robot: bool) -> SnapshotNode {
        SnapshotNode {
            key: "n_sales".to_string(),
            name: "销售主管审批".to_string(),
            node_type: "APPROVAL".to_string(),
            assignee_type: Some(kind.to_string()),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: true,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: "ERROR".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 1,
            config: json!({}),
            assignees: Vec::new(),
        }
    }

    fn cand(name: &str, robot: bool, active: bool) -> Candidate {
        let mut candidate = Candidate::new(Uuid::now_v7(), name, AssigneeKind::Role);
        candidate.is_robot = robot;
        candidate.active = active;
        candidate
    }

    /// ① 角色配了但查不到 → ROLE_NOT_FOUND（去改节点，不是去加人）
    #[test]
    fn role_missing_is_not_found() {
        let signal = Signal {
            targets: 1,
            ..Signal::default()
        };
        assert_eq!(
            classify("ROLE", false, 0, signal),
            Reason::RoleNotFound,
            "角色 id 查不到 ⇒ 角色被删 / 改名"
        );
    }

    /// ② 角色存在但 0 成员 → ROLE_NO_MEMBER（新环境开箱第一案）
    #[test]
    fn empty_role_is_no_member() {
        let signal = Signal {
            targets: 1,
            ..Signal::default()
        };
        assert_eq!(classify("ROLE", true, 0, signal), Reason::RoleNoMember);
        assert_eq!(Reason::RoleNoMember.code(), "ROLE_NO_MEMBER");
        assert!(Reason::RoleNoMember.hint("销售主管").contains("销售主管"));
    }

    /// ③ 有成员行但一个候选都没解析出来 → ROLE_MEMBER_UNAVAILABLE（人停用/删除）
    #[test]
    fn role_with_dead_members_is_unavailable() {
        let signal = Signal {
            targets: 1,
            candidates: 0,
            ..Signal::default()
        };
        assert_eq!(
            classify("ROLE", true, 3, signal),
            Reason::RoleMemberUnavailable
        );
    }

    /// ④ 候选全是停用 → ROLE_MEMBER_UNAVAILABLE（不是「没配」）
    #[test]
    fn all_inactive_are_unavailable() {
        let signal = Signal {
            targets: 1,
            candidates: 2,
            alive: 0,
            eligible: 0,
            non_self: 0,
        };
        assert_eq!(
            classify("ROLE", true, 2, signal),
            Reason::RoleMemberUnavailable
        );
    }

    /// ⑤ 候选全是机器人且节点不允许 → ROBOT_ONLY
    #[test]
    fn robot_only_is_reported() {
        let signal = Signal {
            targets: 1,
            candidates: 1,
            alive: 1,
            eligible: 0,
            non_self: 0,
        };
        assert_eq!(classify("ROLE", true, 1, signal), Reason::RobotOnly);
    }

    /// ⑥ 候选只有发起人本人 → SELF_ONLY（新环境「兜底超管=制单人」的真实成因）
    #[test]
    fn self_only_is_reported() {
        let signal = Signal {
            targets: 1,
            candidates: 1,
            alive: 1,
            eligible: 1,
            non_self: 0,
        };
        assert_eq!(classify("ROLE", true, 1, signal), Reason::SelfOnly);
        assert!(Reason::SelfOnly.hint("销售主管").contains("其他"));
    }

    /// ⑦ 非 ROLE 来源各自有专属原因（别再统一报「没人」）
    #[test]
    fn other_sources_have_specific_reasons() {
        let empty = Signal::default();
        assert_eq!(classify("USER", false, 0, empty), Reason::UserUnavailable);
        assert_eq!(
            classify("DEPT_LEADER", false, 0, empty),
            Reason::DeptNoLeader
        );
        assert_eq!(
            classify("INITIATOR_SUPERIOR", false, 0, empty),
            Reason::SuperiorNotFound
        );
        assert_eq!(
            classify("TASK_OWNER", false, 0, empty),
            Reason::TaskPartyMissing
        );
        assert_eq!(
            classify("FORM_VAR", false, 0, empty),
            Reason::FormVarMissing
        );
        assert_eq!(
            classify("", false, 0, empty),
            Reason::NodeNotConfigured,
            "空来源 = 节点没配"
        );
    }

    /// ⑧ `Signal::of` 的四层计数与 `finalize` 口径一致（停用 / 机器人 / 自审）
    #[test]
    fn signal_counts_layers() {
        let spec = node("ROLE", false);
        let initiator = Uuid::now_v7();
        let mut me = cand("发起人", false, true);
        me.user_id = initiator;
        let candidates = vec![
            me,
            cand("停用的人", false, false),
            cand("机器人", true, true),
        ];
        let signal = Signal::of(&spec, &candidates, initiator);
        assert_eq!(signal.candidates, 3);
        assert_eq!(signal.alive, 2, "停用的那位不算");
        assert_eq!(signal.eligible, 1, "机器人被过滤");
        assert_eq!(signal.non_self, 0, "唯一的活人就是发起人");
        assert_eq!(
            classify("ROLE", true, 3, signal),
            Reason::SelfOnly,
            "落到 SELF_ONLY 而不是「没人」"
        );
    }

    /// ⑨ 内控全剔专用构造：candidates 全算「可用」，只是都不是非本人
    #[test]
    fn all_excluded_signal_is_self_only() {
        let signal = Signal::all_excluded(1);
        assert_eq!(classify("ROLE", true, 1, signal), Reason::SelfOnly);
    }

    /// ⑩ 载荷字段集合就是前端契约（少一个字段前端就读不到）
    #[test]
    fn payload_shape_is_stable() {
        let payload = NoAssigneePayload {
            flow_instance_id: Some("i".into()),
            node_id: None,
            node_key: "n_sales".into(),
            node_name: "销售主管审批".into(),
            assignee_type: "ROLE".into(),
            role_id: Some("r".into()),
            role_code: Some("SALES_SUPERVISOR".into()),
            role_name: Some("销售主管".into()),
            member_count: 0,
            usable_member_count: 0,
            candidate_count: 0,
            reason: "ROLE_NO_MEMBER".into(),
            reason_text: "该角色下没有任何在职成员".into(),
            excluded: Vec::new(),
            escalation_chain: Vec::new(),
            escalation_reason: None,
            escalated_from_role: None,
            fix_hint: "去加人".into(),
        };
        let json = payload.to_json();
        for key in [
            "flow_instance_id",
            "node_key",
            "node_name",
            "assignee_type",
            "role_id",
            "role_code",
            "role_name",
            "member_count",
            "usable_member_count",
            "candidate_count",
            "reason",
            "reason_text",
            "fix_hint",
        ] {
            assert!(json.get(key).is_some(), "载荷缺字段 {key}: {json}");
        }
        assert!(json.get("node_id").is_none(), "None 字段不序列化");
        assert!(json.get("excluded").is_none(), "空数组不序列化");
    }

    /// ⑪ `attach` 只认 65018，且不覆盖已有载荷
    #[test]
    fn attach_is_scoped_and_idempotent() {
        let mut other = ApiError::new(400, 65005, "字段非法");
        attach(&mut other, json!({"x": 1}));
        assert!(other.data.is_none(), "非 65018 不许挂审批人载荷");

        let mut error = ApiError::new(422, error_code::FLOW_NO_ASSIGNEE, "没人");
        attach(&mut error, json!({"reason": "ROLE_NO_MEMBER"}));
        attach(&mut error, json!({"reason": "覆盖"}));
        assert_eq!(
            error.data.as_ref().and_then(|v| v.get("reason")),
            Some(&json!("ROLE_NO_MEMBER")),
            "第二次 attach 不该覆盖第一次"
        );
    }

    /// ⑫ `set_node_id` 补齐实例节点 id，且不动别的字段
    #[test]
    fn set_node_id_patches_payload() {
        let mut error = ApiError::new(422, error_code::FLOW_NO_ASSIGNEE, "没人")
            .with_data(json!({"node_key": "n_sales", "reason": "SELF_ONLY"}));
        let node_id = Uuid::now_v7();
        set_node_id(&mut error, node_id);
        let data = error.data.expect("载荷");
        assert_eq!(data.get("node_id"), Some(&json!(node_id.to_string())));
        assert_eq!(data.get("node_key"), Some(&json!("n_sales")));
    }

    /// ⑬ `configured_targets` 认得 ROLE 的配置个数（0 角色 ⇒ NODE_NOT_CONFIGURED）
    #[test]
    fn empty_role_list_means_not_configured() {
        let mut spec = node("ROLE", false);
        spec.assignees = Vec::new();
        assert_eq!(configured_targets(&spec), 0);
        assert_eq!(
            classify("ROLE", false, 0, Signal::of(&spec, &[], Uuid::now_v7())),
            Reason::NodeNotConfigured
        );
    }
}
