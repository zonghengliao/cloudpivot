//! 状态机边配置的**管理面 + 审批化**（【D3】流程配置化 · 阶段 3 出口 / 卡 t_217a95ad）。
//!
//! 内核在 [`crate::business::shared::state_machine`]（D1）：`BASE_EDGES` 硬约束白名单 +
//! `biz_flow_state_edge` 配置表 + 运行时 `effective_edges()` 合并 + 唯一写入入口
//! `upsert_edge_config` / `delete_edge_config` / `list_edge_config`。
//! **本模块一行合并 / 校验逻辑都不重复实现**，只做三件事：
//!
//! | # | 做什么 | 落点 |
//! |---|--------|------|
//! | 1 | HTTP 管理面（查 / 改 / 删） | [`list_state_edges`] / [`upsert_state_edge`] / [`delete_state_edge`] |
//! | 2 | 改配置**本身走审批流** | [`start_change_approval`] + [`on_settled`]（回调里调内核） |
//! | 3 | 「待变更内容」的载体 | 审批单的 `form_data`（不另建影子表） |
//!
//! # 为什么改配置要走审批（卡面原话：「流程全部用工作任务和审批流来实现」）
//!
//! D1 的写入三道门是「超管 + 二次确认 + 留痕」——**一个人点两下就能改全公司的审批路径**。
//! 审批化之后：提交 → 复核人看得到「要改哪条边、改成什么、为什么改」→ 同意才真正落库；
//! 驳回 / 撤回则配置一分不动。审批单永久留在审批中心，比一行 `biz_status_log` 更容易被追问时找到。
//!
//! ```text
//! PUT /business/flow/state-edge
//!   ├─ ① 按内核口径「试写 + 回滚」（dry-run）：非法补丁在**提交阶段**就 400，
//!   │     不会出现「审批通过了才发现配不了」
//!   ├─ ② 有已发布的 flow_edge_config 流程 → 起一张审批单（form_data = 待变更内容），
//!   │     同一 (entity, from, to) 同时只能有一张在跑的审批单（uk_biz_flow_instance_biz_active）
//!   └─ ③ 流程被下线 → 不能静默放行：409（复用 approval_gate 的「默认拒绝」），
//!         只有持 `business:erp:skip-approval` + 理由 ≥10 字才直接生效并留痕
//!
//! 审批回调（flow::handler 的同意 / 驳回 / 撤回三处 + 提交后自动完成）
//!   APPROVED            → 以**发起人**的身份调内核 upsert_edge_config / delete_edge_config
//!   REJECTED/WITHDRAWN  → 什么也不写，留一条 biz_status_log（配置未变更）
//! ```
//!
//! # 为什么回调里要「替发起人构造 claims」
//!
//! 内核的入口签名是 `upsert_edge_config(ex, claims, patch)`，`claims` 决定两件事：
//! ① 是否超管（唯一有权改状态机的人）；② 审计行的 `operator_id / operator_name`。
//! 审批回调时没有 HTTP 请求、没有令牌，所以这里**按发起人的真实账号信息**重建一份 claims
//! （角色 / 权限从库里现读，权限码口径与登录时完全一致），并在**审批通过的那一刻**重新核一次
//! 「发起人还是不是超管」—— 不是就**不生效** + WARN + 留痕（撤销权限之后，之前提的单不该还能落地）。
//!
//! # 边界（刻意不做的事）
//!
//! - **不改内核的合并 / 校验**：`upsert_edge_config` 里的「只能从 BASE 边里挑 / 不能禁用 mandatory /
//!   不能放宽 requires_reason」原样生效，本模块只做参数解析与错误映射；
//! - **不给 handler 开写 `biz_flow_state_edge` 的口子**：唯一写入口仍是内核那两个函数；
//! - **不把 `FLOW_EDGE_CFG` 加进 `flow::internal_control` 的受控清单**（那份清单是 6 个账务业务）。
//!   语义上「改配置」是管理员的内控动作：单超管环境要能自助完成，但**必须留一张审批单**
//!   （谁发起、谁复核、何时生效）。要双人复核，把 `FLOW_CONFIG_APPROVER` 角色绑上人即可 ——
//!   绑了人之后 `FALLBACK_ADMIN` 不再兜底，审批人就不是发起人自己了。
//! - **不动 `flow_edge_config` 的图**：它是 `is_system = TRUE` 的预置模板，改图 = 换流程（另开卡）。

use axum::Json;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::business::erp::approval_gate;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine::{
    self, BaseEdge, Edge, EdgeConfigPatch, EdgeConfigRow, Entity, Operator,
};
use crate::shared::error_code;
use crate::utils;

/// 读配置的权限码（与 `V0206` 种进 `sys_menu.auth_code` 的值逐字一致）。
pub const PERM_LIST: &str = "business:flow:state-edge:list";
/// 改 / 删配置的权限码（同上；`V0206` 只授 SUPER_ADMIN）。
pub const PERM_MANAGE: &str = "business:flow:state-edge:manage";

/// 改配置审批的业务类型（`biz_flow_def.biz_type`）。
pub const BIZ_TYPE: &str = "FLOW_EDGE_CFG";
/// 改配置审批的流程编码（`biz_flow_def.flow_code`）。
pub const FLOW_CODE: &str = "flow_edge_config";

/// 变更动作（落进审批单 `form_data.action`）。
pub const ACTION_UPSERT: &str = "UPSERT";
/// 变更动作：删除配置（回到 BASE_EDGES 行为）。
pub const ACTION_DELETE: &str = "DELETE";

// ==================== 请求 ====================

/// `PUT /business/flow/state-edge` 的请求体。
///
/// `confirm` 是**二次确认**（与内核同一口径）：改状态机配置 = 改流程规则，必须显式传 `true`。
/// `skip_approval` / `skip_reason` 以 `#[serde(flatten)]` 混进来（与采购 / 收货等 6 个入口同款），
/// 所以老调用方不传这两个字段时行为完全不变。
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeUpsertReq {
    /// 实体类型（`STOCK_IN` / `PURCHASE_ORDER` / …，见 `Entity::ALL`）。
    pub entity: String,
    /// 变更前状态。
    pub from_status: String,
    /// 变更后状态。
    pub to_status: String,
    /// 是否启用（`false` = 禁用这条可选边）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 是否要求原因（只能加不能减）。
    #[serde(default)]
    pub requires_reason: bool,
    /// 显示文案（空 = 用 BASE 的默认文案）。
    #[serde(default)]
    pub label: Option<String>,
    /// 追加的权限码。
    #[serde(default)]
    pub auth_code: Option<String>,
    /// 二次确认（必须 `true`）。
    #[serde(default)]
    pub confirm: bool,
    /// 变更理由（写进审批单标题 / 审计留痕；建议填）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段（只有「流程被下线」那条路会用到）。
    #[serde(flatten)]
    pub skip: approval_gate::SkipApprovalReq,
}

impl EdgeUpsertReq {
    /// 转成内核的入参形态（本模块**不**做校验，校验在内核里）。
    pub fn to_patch(&self) -> EdgeConfigPatch {
        EdgeConfigPatch {
            entity: self.entity.clone(),
            from_status: self.from_status.clone(),
            to_status: self.to_status.clone(),
            enabled: self.enabled,
            requires_reason: self.requires_reason,
            label: self.label.clone(),
            auth_code: self.auth_code.clone(),
            // 内核会校验它；透传原值，避免「本层偷偷补 true」把二次确认变成摆设
            confirm: self.confirm,
        }
    }
}

/// `DELETE /business/flow/state-edge` 的查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeDeleteQuery {
    /// 实体类型。
    pub entity: String,
    /// 变更前状态。
    pub from_status: String,
    /// 变更后状态。
    pub to_status: String,
    /// 二次确认（必须 `true`）。
    #[serde(default)]
    pub confirm: bool,
    /// 删除理由（写进审批单标题 / 审计留痕；建议填）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 是否显式跳过审批（只有「流程被下线」那条路会用到）。
    #[serde(default)]
    pub skip_approval: bool,
    /// 跳过审批的理由（≥10 字）。
    #[serde(default)]
    pub skip_reason: Option<String>,
}

impl EdgeDeleteQuery {
    /// 拼成 `SkipApprovalReq`（与采购等入口同一形态，便于复用 `approval_gate` 的判据）。
    fn skip_req(&self) -> approval_gate::SkipApprovalReq {
        approval_gate::SkipApprovalReq {
            skip_approval: self.skip_approval,
            skip_reason: self.skip_reason.clone(),
        }
    }
}

/// `GET /business/flow/state-edge` 的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StateEdgeQuery {
    /// 实体类型（不传 = 9 个实体全列出）。
    #[serde(default)]
    pub entity: Option<String>,
}

// ==================== 响应 ====================

/// 一条边（BASE 侧 / 有效侧共用；`configured` 只在「当前有配置行」时为 `true`）。
#[derive(Debug, Clone, Serialize)]
pub struct EdgeRes {
    /// 变更前状态。
    pub from: String,
    /// 变更后状态。
    pub to: String,
    /// 显示文案（配置覆盖后的值）。
    pub label: String,
    /// 是否硬约束边（`true` = 配置禁不掉）。
    pub mandatory: bool,
    /// 走这条边是否必须填原因（BASE ⊙ 配置，只能加不能减）。
    pub requires_reason: bool,
    /// 配置追加的权限码。
    pub auth_code: Option<String>,
    /// 当前是否有存活配置行。
    pub configured: bool,
    /// 配置是否启用（无边配置时恒 `true`）。
    pub enabled: bool,
}

impl EdgeRes {
    /// 由合并后的有效边构造（`edges` 里出现的都是启用中的边）。
    fn of_effective(edge: &Edge, configured: bool) -> Self {
        Self {
            from: edge.from.to_string(),
            to: edge.to.to_string(),
            label: edge.label.clone(),
            mandatory: edge.mandatory,
            requires_reason: edge.requires_reason,
            auth_code: edge.auth_code.clone(),
            configured,
            enabled: true,
        }
    }

    /// 由 BASE 边构造（**代码里的真相**，配置删不掉）。
    fn of_base(edge: &BaseEdge) -> Self {
        Self {
            from: edge.from.to_string(),
            to: edge.to.to_string(),
            label: edge.meta.default_label.to_string(),
            mandatory: edge.meta.mandatory,
            requires_reason: edge.meta.requires_reason,
            auth_code: None,
            configured: false,
            enabled: true,
        }
    }
}

/// 一行存活配置（`biz_flow_state_edge` 的原样投影，管理端要看「配了什么」）。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigRowRes {
    /// 实体类型。
    pub entity: String,
    /// 变更前状态。
    pub from: String,
    /// 变更后状态。
    pub to: String,
    /// 是否启用。
    pub enabled: bool,
    /// 是否要求原因。
    pub requires_reason: bool,
    /// 显示文案（`None` = 用 BASE 默认）。
    pub label: Option<String>,
    /// 追加的权限码。
    pub auth_code: Option<String>,
}

impl ConfigRowRes {
    /// 由内核的行构造。
    fn of(row: &EdgeConfigRow) -> Self {
        Self {
            entity: row.entity.clone(),
            from: row.from_status.clone(),
            to: row.to_status.clone(),
            enabled: row.enabled,
            requires_reason: row.requires_reason,
            label: row.label.clone(),
            auth_code: row.auth_code.clone(),
        }
    }
}

/// 一张在跑的配置变更审批单（管理端要能显示「这条边正在审批中」）。
#[derive(Debug, Clone, Serialize)]
pub struct PendingChangeRes {
    /// 审批单 id。
    pub instance_id: String,
    /// 审批单号（`FLW-…`，可直接在审批中心搜）。
    pub instance_no: String,
    /// 标题。
    pub title: String,
    /// 动作（`UPSERT` / `DELETE`）。
    pub action: String,
    /// 实体类型。
    pub entity: String,
    /// 变更前状态。
    pub from: String,
    /// 变更后状态。
    pub to: String,
    /// 会变成什么样（人话，例：`禁用（变成 false）`）。
    pub summary: String,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起时间（RFC 3339）。
    pub started_at: String,
}

/// 一个实体的「BASE 边 + 当前配置 + 合并后的有效边」。
#[derive(Debug, Clone, Serialize)]
pub struct EntityEdgesRes {
    /// 实体类型（`STOCK_IN` / …）。
    pub entity: String,
    /// 实体中文名（前端不用再维护一份映射）。
    pub entity_label: String,
    /// 代码里的 BASE 边（真相，配置删不掉）。
    pub base_edges: Vec<EdgeRes>,
    /// 当前存活配置行。
    pub configs: Vec<ConfigRowRes>,
    /// **合并后的有效边**（前端按钮该照这份显示；`effective_edges()` 的投影）。
    pub effective_edges: Vec<EdgeRes>,
    /// 在跑的配置变更审批单。
    pub pending_changes: Vec<PendingChangeRes>,
}

/// `GET /business/flow/state-edge` 的响应（`list` 形态，传了 `entity` 就只有一条）。
#[derive(Debug, Clone, Serialize)]
pub struct StateEdgeListRes {
    /// 各实体的边配置视图。
    pub list: Vec<EntityEdgesRes>,
    /// `list` 长度。
    pub total: usize,
}

/// `PUT` / `DELETE` 的响应：要么「已送审」，要么「已直接生效」。
#[derive(Debug, Clone, Serialize)]
pub struct EdgeChangeRes {
    /// 是否**已经生效**（`false` = 已送审，等审批）。
    pub applied: bool,
    /// 审批单 id（`applied = false` 时必有）。
    pub instance_id: Option<String>,
    /// 审批单号（同上）。
    pub instance_no: Option<String>,
    /// 审批单状态（`PENDING` / `APPROVED`）。
    pub instance_status: Option<String>,
    /// 给人看的结论（含下一步该干什么）。
    pub message: String,
}

impl EdgeChangeRes {
    /// 「已送审」形态。
    fn pending(instance_id: Uuid, instance_no: String, status: String, summary: &str) -> Self {
        Self {
            applied: false,
            instance_id: Some(instance_id.to_string()),
            instance_no: Some(instance_no.clone()),
            instance_status: Some(status),
            message: format!(
                "{summary}：已提交审批 {instance_no}，审批通过后才会生效（驳回或撤回则配置不变）"
            ),
        }
    }

    /// 「已直接生效」形态（流程被下线 + 持码 + 理由齐全的显式跳过）。
    fn applied(summary: &str, why: &str) -> Self {
        Self {
            applied: true,
            instance_id: None,
            instance_no: None,
            instance_status: None,
            message: format!("{summary}：已直接生效（{why}）"),
        }
    }
}

// ==================== HTTP：查 ====================

/// 查状态机边配置（`GET /business/flow/state-edge?entity=STOCK_OUT`）。
///
/// 返回三份东西，**一份不重写**：
/// - `base_edges`：`Entity::base_edges()`（代码里的真相）；
/// - `configs`：`state_machine::list_edge_config()`（库里配了什么）；
/// - `effective_edges`：`state_machine::effective_edges()`（合并后前端该照它显示按钮）。
///
/// # 异常情况
///
/// - 400 + `65005`：`entity` 不是 9 个实体之一（不静默返回空列表）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/state-edge",
    method = "GET",
    permission = "business:flow:state-edge:list"
)]
#[nik::get("/business/flow/state-edge")]
pub async fn list_state_edges(Query(query): Query<StateEdgeQuery>) -> ResResult<StateEdgeListRes> {
    let entity = match query
        .entity
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        None => None,
        Some(raw) => Some(parse_entity(raw)?),
    };
    let targets: Vec<Entity> = match entity {
        Some(item) => vec![item],
        None => Entity::ALL.to_vec(),
    };
    let configs = state_machine::list_edge_config(db(), entity)
        .await
        .map_err(|error| utils::db_error(error, "查询状态机配置"))?;
    let pending = pending_changes(entity).await?;
    let mut list = Vec::with_capacity(targets.len());
    for item in targets {
        list.push(entity_view(item, &configs, &pending).await);
    }
    Ok(Res::ok(StateEdgeListRes {
        total: list.len(),
        list,
    }))
}

/// 组装单个实体的视图（有效边走内核的 `effective_edges`，不在这里合并）。
async fn entity_view(
    entity: Entity,
    configs: &[EdgeConfigRow],
    pending: &[PendingChangeRes],
) -> EntityEdgesRes {
    let effective = state_machine::effective_edges(db(), entity).await;
    let rows: Vec<ConfigRowRes> = configs
        .iter()
        .filter(|row| row.entity == entity.as_str())
        .map(ConfigRowRes::of)
        .collect();
    // 有效边先算好（它要用 `rows`），再把 `rows` 移进 DTO（避免「借用中移动」）
    let effective_edges: Vec<EdgeRes> = effective
        .iter()
        .map(|edge| {
            let configured = rows
                .iter()
                .any(|row| row.from == edge.from && row.to == edge.to);
            EdgeRes::of_effective(edge, configured)
        })
        .collect();
    EntityEdgesRes {
        entity: entity.as_str().to_string(),
        entity_label: entity.label().to_string(),
        base_edges: entity.base_edges().iter().map(EdgeRes::of_base).collect(),
        configs: rows,
        effective_edges,
        pending_changes: pending
            .iter()
            .filter(|item| item.entity == entity.as_str())
            .cloned()
            .collect(),
    }
}

// ==================== HTTP：改 ====================

/// 改一条边的配置（`PUT /business/flow/state-edge`）。
///
/// **真正落库的是审批回调**：这里只做三件事 —— 试写校验、起审批单、把「待变更内容」写进 `form_data`。
///
/// # 异常情况
///
/// - 403：非超管（内核判）/ 不是流程配置管理员（注解判）；
/// - 400：二次确认缺失 / 未知实体 / 不是 BASE 边 / 禁用 mandatory / 放宽 requires_reason
///   （全部来自内核，本层只透传）；
/// - 409：同一条边已有在跑的审批单（报文带审批单号）；或流程被下线且没请求跳过。
#[nik::permission(
    path = "/business/flow/state-edge",
    method = "PUT",
    permission = "business:flow:state-edge:manage"
)]
#[nik::put("/business/flow/state-edge")]
pub async fn upsert_state_edge(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<EdgeUpsertReq>,
) -> ResResult<EdgeChangeRes> {
    let actor = me_of(&claims)?;
    let patch = req.to_patch();
    // ① 试写 + 回滚：把内核的 400 / 403 提前到提交阶段（审批通过后才发现配不了是更坏的用户体验）
    dry_run_upsert(&claims, &patch).await?;
    let change = EdgeChange::Upsert(patch.clone());
    let summary = change.summary();
    let gate = approval_gate::ApprovalGate::new(&req.skip, &claims);
    // ② 流程在 → 必须送审；流程被下线 → 只能「持码 + 理由」显式跳过（复用 approval_gate 判据）
    let Some(flow) = published_flow().await? else {
        let skip_reason = approval_gate::authorize_missing_flow(BIZ_TYPE, FLOW_CODE, &gate)?;
        tracing::warn!(
            entity = %patch.entity,
            from = %patch.from_status,
            to = %patch.to_status,
            %skip_reason,
            "状态机配置变更：没有已发布的 flow_edge_config 流程，按「显式跳过审批」直接生效"
        );
        state_machine::upsert_edge_config(db(), &claims, &patch).await?;
        note_change(&claims, &change, "SKIPPED", &skip_reason).await;
        return Ok(Res::ok(EdgeChangeRes::applied(&summary, &skip_reason)));
    };
    approval_gate::ensure_not_skipped(FLOW_CODE, &gate)?;
    let started = start_change_approval(
        &flow,
        &change,
        actor,
        req.reason.as_deref(),
        Some(claims.username.clone().unwrap_or_default()),
    )
    .await?;
    maybe_settle_auto_approved(&started).await;
    Ok(Res::ok(EdgeChangeRes::pending(
        started.instance_id,
        started.instance_no,
        started.instance_status,
        &summary,
    )))
}

/// 删掉一条边的配置（`DELETE /business/flow/state-edge?entity=&from_status=&to_status=&confirm=true`）。
///
/// 语义是「回到 BASE_EDGES 行为」（软删）。与 `PUT` 同款：**落库在审批回调里**。
///
/// # 异常情况
///
/// - 400：二次确认缺失 / 未知实体 / 不是 BASE 边（内核判）；
/// - 404：本来就没有配置（重复删除不产生审批单 —— 没有意义的审批单只是噪音）；
/// - 409：同一条边已有在跑的审批单。
#[nik::permission(
    path = "/business/flow/state-edge",
    method = "DELETE",
    permission = "business:flow:state-edge:manage"
)]
#[nik::delete("/business/flow/state-edge")]
pub async fn delete_state_edge(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<EdgeDeleteQuery>,
) -> ResResult<EdgeChangeRes> {
    let actor = me_of(&claims)?;
    let entity = parse_entity(query.entity.trim())?;
    let from = query.from_status.trim().to_string();
    let to = query.to_status.trim().to_string();
    // ① 试写 + 回滚（`delete_edge_config` 本身幂等：没配置时返回 false）
    dry_run_delete(&claims, &entity, &from, &to, query.confirm).await?;
    let exists = state_machine::list_edge_config(db(), Some(entity))
        .await
        .map_err(|error| utils::db_error(error, "查询状态机配置"))?
        .into_iter()
        .any(|row| row.from_status == from && row.to_status == to);
    if !exists {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!(
                "{} 的 {from}→{to} 没有配置行可删：它已经在用 BASE_EDGES 的默认行为",
                entity.as_str()
            ),
        ));
    }
    let change = EdgeChange::Delete {
        entity: entity.as_str().to_string(),
        from: from.clone(),
        to: to.clone(),
    };
    let summary = change.summary();
    let skip = query.skip_req();
    let gate = approval_gate::ApprovalGate::new(&skip, &claims);
    let Some(flow) = published_flow().await? else {
        let skip_reason = approval_gate::authorize_missing_flow(BIZ_TYPE, FLOW_CODE, &gate)?;
        tracing::warn!(
            entity = entity.as_str(),
            %from, %to, %skip_reason,
            "状态机配置删除：没有已发布的 flow_edge_config 流程，按「显式跳过审批」直接生效"
        );
        state_machine::delete_edge_config(db(), &claims, entity.as_str(), &from, &to).await?;
        note_change(&claims, &change, "SKIPPED", &skip_reason).await;
        return Ok(Res::ok(EdgeChangeRes::applied(&summary, &skip_reason)));
    };
    approval_gate::ensure_not_skipped(FLOW_CODE, &gate)?;
    let started = start_change_approval(
        &flow,
        &change,
        actor,
        query.reason.as_deref(),
        Some(claims.username.clone().unwrap_or_default()),
    )
    .await?;
    maybe_settle_auto_approved(&started).await;
    Ok(Res::ok(EdgeChangeRes::pending(
        started.instance_id,
        started.instance_no,
        started.instance_status,
        &summary,
    )))
}

// ==================== 待变更内容（审批单 form_data 的载荷） ====================

/// 一条待变更内容：`UPSERT`（改 / 加配置）或 `DELETE`（删配置）。
#[derive(Debug, Clone)]
pub enum EdgeChange {
    /// 写一行配置（内核 `upsert_edge_config`）。
    Upsert(EdgeConfigPatch),
    /// 删一行配置（内核 `delete_edge_config`）。
    Delete {
        /// 实体类型。
        entity: String,
        /// 变更前状态。
        from: String,
        /// 变更后状态。
        to: String,
    },
}

impl EdgeChange {
    /// 实体类型。
    pub fn entity(&self) -> &str {
        match self {
            EdgeChange::Upsert(patch) => &patch.entity,
            EdgeChange::Delete { entity, .. } => entity,
        }
    }

    /// 变更前状态。
    pub fn from(&self) -> &str {
        match self {
            EdgeChange::Upsert(patch) => &patch.from_status,
            EdgeChange::Delete { from, .. } => from,
        }
    }

    /// 变更后状态。
    pub fn to(&self) -> &str {
        match self {
            EdgeChange::Upsert(patch) => &patch.to_status,
            EdgeChange::Delete { to, .. } => to,
        }
    }

    /// 动作码（`UPSERT` / `DELETE`）。
    pub fn action(&self) -> &'static str {
        match self {
            EdgeChange::Upsert(_) => ACTION_UPSERT,
            EdgeChange::Delete { .. } => ACTION_DELETE,
        }
    }

    /// 一句话说明会变成什么样（审批单标题 / 响应文案都用它）。
    pub fn summary(&self) -> String {
        match self {
            EdgeChange::Upsert(patch) => {
                let mut parts: Vec<String> = Vec::new();
                parts.push(if patch.enabled {
                    "启用".to_string()
                } else {
                    "禁用".to_string()
                });
                if patch.requires_reason {
                    parts.push("要求填原因".to_string());
                }
                if let Some(label) = patch
                    .label
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    parts.push(format!("文案「{label}」"));
                }
                if let Some(code) = patch
                    .auth_code
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    parts.push(format!("权限码 {code}"));
                }
                format!(
                    "{} {}→{} {}",
                    patch.entity.trim(),
                    patch.from_status.trim(),
                    patch.to_status.trim(),
                    parts.join(" / ")
                )
            }
            EdgeChange::Delete { entity, from, to } => {
                format!("{entity} {from}→{to} 删除配置（回到 BASE_EDGES）")
            }
        }
    }

    /// 落 `form_data` 的 JSON（审批单就是「待变更内容」的载体，不另建影子表）。
    pub fn to_form_data(&self, reason: Option<&str>) -> JsonValue {
        match self {
            EdgeChange::Upsert(patch) => json!({
                "action": ACTION_UPSERT,
                "entity": patch.entity.trim(),
                "from_status": patch.from_status.trim(),
                "to_status": patch.to_status.trim(),
                "enabled": patch.enabled,
                "requires_reason": patch.requires_reason,
                "label": patch.label.clone(),
                "auth_code": patch.auth_code.clone(),
                "reason": reason.map(str::trim).filter(|text| !text.is_empty()),
                // 二次确认在提交时已校验过一次；回调里重新置 true（内核要求，且审批通过本身就是二次确认）
                "confirm": true,
            }),
            EdgeChange::Delete { entity, from, to } => json!({
                "action": ACTION_DELETE,
                "entity": entity,
                "from_status": from,
                "to_status": to,
                "reason": reason.map(str::trim).filter(|text| !text.is_empty()),
            }),
        }
    }

    /// 从 `form_data` 还原（还原失败 = 脏行，回调不猜，直接 WARN 跳过）。
    pub fn from_form_data(data: &JsonValue) -> Option<EdgeChange> {
        let text = |key: &str| -> Option<String> {
            data.get(key)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let flag = |key: &str| {
            data.get(key)
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        let entity = text("entity")?;
        let from = text("from_status")?;
        let to = text("to_status")?;
        match text("action").as_deref() {
            Some(ACTION_UPSERT) => Some(EdgeChange::Upsert(EdgeConfigPatch {
                entity,
                from_status: from,
                to_status: to,
                enabled: data
                    .get("enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
                requires_reason: flag("requires_reason"),
                label: text("label"),
                auth_code: text("auth_code"),
                confirm: true,
            })),
            Some(ACTION_DELETE) => Some(EdgeChange::Delete { entity, from, to }),
            _ => None,
        }
    }

    /// 「这条边」的确定性业务 id：同一 `(entity, from, to)` 永远同一个 uuid。
    ///
    /// 这样 `uk_biz_flow_instance_biz_active(biz_type, biz_id)` 天然保证
    /// **同一条边同一时刻只有一张在跑的审批单**，不同边的审批互不影响。
    pub fn biz_id(&self) -> Uuid {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"cloudpivot.flow_edge_config|");
        hasher.update(self.entity().as_bytes());
        hasher.update(b"|");
        hasher.update(self.from().as_bytes());
        hasher.update(b">");
        hasher.update(self.to().as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        // 标成 v8（自定义）+ RFC 4122 variant：一眼能看出不是随机 uuid
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Uuid::from_bytes(bytes)
    }
}

// ==================== 审批：发起 ====================

/// 已发布的 `flow_edge_config` 流程（`None` = 没配 / 被下线）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的改配置审批流程」。
///
/// 排序口径与 `purchase_bridge::published_po_flow` 一致（`biz_type` 精确命中优先 → `is_default`
/// → 版本号 → 更新时间），避免「同 biz_type 两条流程时挑哪条」不可判定。
pub async fn published_flow() -> std::result::Result<Option<EdgeFlow>, ApiError> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![
                Value::Text(BIZ_TYPE.to_string()),
                Value::Text(FLOW_CODE.to_string()),
                Value::Text(BIZ_TYPE.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询状态机配置审批流程"))?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let read = |error: DbError| utils::db_error(error, "读流程定义");
    Ok(Some(EdgeFlow {
        def_id: row.get_typed("def_id").map_err(read)?,
        flow_code: row.get_typed("flow_code").map_err(read)?,
        flow_name: row.get_typed("flow_name").map_err(read)?,
        biz_type: row.get_typed("biz_type").map_err(read)?,
        version_id: row.get_typed("version_id").map_err(read)?,
        version_no: row.get_typed("version_no").map_err(read)?,
    }))
}

/// 起一张配置变更审批单的结果。
#[derive(Debug, Clone)]
pub struct StartedApproval {
    /// 审批单 id。
    pub instance_id: Uuid,
    /// 审批单号。
    pub instance_no: String,
    /// 审批单状态（全被跳过时可能直接 `APPROVED`）。
    pub instance_status: String,
}

/// 把「待变更内容」落成一张审批单（事务内发起，提交后发通知）。
///
/// # 异常情况
///
/// - 409：同一条边已有在跑的审批单（报文带审批单号）；
/// - 422 + `65018`：审批人全部被内控排除（引擎原样透出）；
/// - 500：数据库异常。
pub async fn start_change_approval(
    flow: &EdgeFlow,
    change: &EdgeChange,
    actor: Uuid,
    reason: Option<&str>,
    actor_name: Option<String>,
) -> std::result::Result<StartedApproval, ApiError> {
    let biz_id = change.biz_id();
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(actor)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((actor_name.unwrap_or_default(), None));
    let title: String = format!("状态机配置变更：{}", change.summary())
        .chars()
        .take(200)
        .collect();
    let biz_no: String = format!("{} {}>{}", change.entity(), change.from(), change.to());
    let start = engine::StartReq {
        instance_id: Uuid::now_v7(),
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(biz_id),
        biz_no: Some(biz_no),
        title,
        initiator_id: actor,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: change.to_form_data(reason),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启配置变更审批事务"))?;
    // 同一条边只能有一张在跑的审批单（库里有部分唯一索引兜底，这里先查一次给人话报文）
    if flow_model::biz_instance_running_in(&tx, BIZ_TYPE, biz_id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑的配置审批单"))?
    {
        let _ = tx.rollback().await;
        let instance_no = running_instance_no(biz_id).await?;
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "{} 这条边已有一张在跑的配置变更审批单（{instance_no}）：\
                 请等它审完，或从审批中心撤回后再提交（同一条边不允许两张并行的审批）",
                change.summary()
            ),
        ));
    }
    let outcome = engine::start(&tx, &start).await;
    let mut out = match outcome {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("发起配置变更审批单失败：{error}"),
            ));
        }
    };
    if let Some(rejected) = out.rejected.take() {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!(
                "{}（配置变更审批的复核人：请把 `FLOW_CONFIG_APPROVER` 角色绑一个不是发起人的账号）",
                rejected.msg
            ),
        ));
    }
    let instance_id = out.instance_id.unwrap_or_default();
    let instance_status = out.instance_status.clone();
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交配置变更审批事务"))?;
    // 锁外副作用（审批通知 / 知会）必须在提交之后发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    let instance_no = instance_no_of(instance_id).await?;
    tracing::info!(
        %instance_id,
        instance_no = %instance_no,
        change = %change.summary(),
        "状态机配置变更已提交审批"
    );
    Ok(StartedApproval {
        instance_id,
        instance_no,
        instance_status,
    })
}

/// 流程里没有审批节点（全被跳过）时，实例在同一事务里就走完了 → 提交后立刻按「通过」回调。
async fn maybe_settle_auto_approved(started: &StartedApproval) {
    if started.instance_status.eq_ignore_ascii_case("APPROVED") {
        on_settled(Some(started.instance_id), "APPROVED").await;
    }
}

// ==================== 审批：回调 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeVerdict {
    /// 同意 → 真正写 `biz_flow_state_edge`。
    Apply,
    /// 驳回 / 撤回 / 终止 → 配置一分不动。
    Drop,
}

impl ChangeVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ChangeVerdict::Apply => "APPLY",
            ChangeVerdict::Drop => "DROP",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动配置）。
pub fn change_verdict(instance_status: &str) -> Option<ChangeVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(ChangeVerdict::Apply),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(ChangeVerdict::Drop),
        _ => None,
    }
}

/// 审批单落终态 → 真正改（或不改）状态机配置（`FlowCallback` 身份）。
///
/// 由 `flow::handler::{approve_task, reject_task, withdraw_instance}` 在**审批事务提交之后**调用；
/// 也由本模块在「实例自动通过」时调用。**不返回错误**：审批结论已经落库，配置侧失败只记日志
/// （与 `purchase_bridge::on_po_settled` 同口径）—— 但一定会留一条 `biz_status_log`，
/// 让「审批过了却没生效」这件事可被追溯。
pub async fn on_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = change_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(entity_id)) => {
            tracing::info!(
                %instance_id,
                verdict = verdict.as_str(),
                %entity_id,
                "状态机配置变更审批结论已回调（配置已落库）"
            );
            Some(entity_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                error = ?error,
                "状态机配置变更审批回调失败（审批结论已落库，配置侧未生效）"
            );
            None
        }
    }
}

/// [`on_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(
    instance_id: Uuid,
    verdict: ChangeVerdict,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "读审批实例"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(change) = EdgeChange::from_form_data(&instance.form_data) else {
        tracing::warn!(
            %instance_id,
            instance_no = %instance.instance_no,
            data = %instance.form_data,
            "配置变更审批单的 form_data 不完整（不是本模块发起的？）→ 不动配置"
        );
        return Ok(None);
    };
    let Some(entity) = Entity::parse(change.entity()) else {
        tracing::warn!(
            %instance_id,
            entity = change.entity(),
            "配置变更审批单的实体类型未知 → 不动配置"
        );
        return Ok(None);
    };
    let edge_id = change.biz_id();
    let detail = change.summary();
    if verdict == ChangeVerdict::Drop {
        tracing::warn!(
            %instance_id,
            instance_no = %instance.instance_no,
            status = %instance.status,
            change = %detail,
            "配置变更审批未通过（驳回 / 撤回 / 终止）：状态机配置一分没动"
        );
        note_approval(
            &instance,
            entity,
            &change,
            edge_id,
            "REJECTED",
            &format!(
                "审批 {} {}：配置未变更（{detail}）；驳回 / 撤回不影响现有行为",
                instance.instance_no,
                change_verdict_label(&instance.status)
            ),
        )
        .await;
        return Ok(None);
    }
    // 审批通过 → 以**发起人**的身份调内核（唯一写入入口，一行 SQL 都不另写）
    let claims = match super_admin_claims_of(instance.initiator_id).await {
        Ok(claims) => claims,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                initiator = %instance.initiator_id,
                error = ?error,
                "配置变更审批通过，但发起人已不是超级管理员 → 不生效（撤销权限后不该还能落地）"
            );
            note_approval(
                &instance,
                entity,
                &change,
                edge_id,
                "APPROVED",
                &format!(
                    "审批 {} 通过但**未生效**：发起人已不是超级管理员（{}）；\
                     请由现任超管重新提交",
                    instance.instance_no, error.msg
                ),
            )
            .await;
            return Ok(None);
        }
    };
    let applied = match &change {
        EdgeChange::Upsert(patch) => state_machine::upsert_edge_config(db(), &claims, patch)
            .await
            .map(|_| ()),
        EdgeChange::Delete { entity, from, to } => {
            state_machine::delete_edge_config(db(), &claims, entity, from, to)
                .await
                .map(|_| ())
        }
    };
    match applied {
        Ok(()) => {
            // 内核已写「改前 → 改后」的审计；这里再补一行「是哪张审批单让它生效的」
            note_approval(
                &instance,
                entity,
                &change,
                edge_id,
                "APPLIED",
                &format!("审批 {} 通过，配置已生效（{detail}）", instance.instance_no),
            )
            .await;
            Ok(Some(edge_id))
        }
        Err(error) => {
            tracing::warn!(
                %instance_id,
                instance_no = %instance.instance_no,
                error = ?error,
                "配置变更审批通过但内核拒绝写入（配置未生效）"
            );
            note_approval(
                &instance,
                entity,
                &change,
                edge_id,
                "APPROVED",
                &format!(
                    "审批 {} 通过但**未生效**：{} {}（{}）",
                    instance.instance_no, error.code, error.msg, detail
                ),
            )
            .await;
            Ok(None)
        }
    }
}

/// 实例状态的中文短名（审计文案用）。
fn change_verdict_label(status: &str) -> &'static str {
    match status.trim().to_ascii_uppercase().as_str() {
        "REJECTED" => "驳回",
        "WITHDRAWN" => "发起人撤回",
        "CANCELLED" => "终止",
        _ => "未通过",
    }
}

/// 写一行「配置变更审批」的审计备注（`biz_status_log`，只增不改）。
///
/// 失败只 WARN：审计写不进去不该把审批回调打崩（审批结论已经落库）。
async fn note_approval(
    instance: &flow_model::InstanceRow,
    entity: Entity,
    change: &EdgeChange,
    edge_id: Uuid,
    state: &str,
    reason: &str,
) {
    let operator = Operator::named(
        Some(instance.initiator_id),
        instance_title(&instance.initiator_name),
    );
    if let Err(error) = state_machine::note_edge_config(
        db(),
        entity,
        change.from(),
        change.to(),
        edge_id,
        state,
        state,
        operator,
        reason,
    )
    .await
    {
        tracing::warn!(
            instance_no = %instance.instance_no,
            error = ?error,
            "写配置变更审计失败（审批结论已落库）"
        );
    }
}

/// 发起人姓名兜底（`None` / 空串 → 空）。
fn instance_title(name: &Option<String>) -> String {
    name.as_deref().unwrap_or_default().to_string()
}

/// 写一行「跳过审批」的审计（直接生效那条路）。
async fn note_change(claims: &TokenClaims, change: &EdgeChange, state: &str, reason: &str) {
    let Some(entity) = Entity::parse(change.entity()) else {
        return;
    };
    if let Err(error) = state_machine::note_edge_config(
        db(),
        entity,
        change.from(),
        change.to(),
        change.biz_id(),
        state,
        state,
        Operator::from(claims),
        reason,
    )
    .await
    {
        tracing::warn!(error = ?error, reason, "写配置变更审计失败（配置已生效）");
    }
}

/// 按发起人的**真实账号信息**重建一份超管 claims（角色 / 权限从库里现读）。
///
/// 与登录时的口径完全一致（`system::auth::handler` 用同一个 `load_permissions`），
/// 只是没有令牌外壳。发起人已不是超管 → 403，回调据此**不生效**。
async fn super_admin_claims_of(user_id: Uuid) -> std::result::Result<TokenClaims, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT r.role_code AS role_code FROM sys_user_role ur \
               JOIN sys_role r ON r.id = ur.role_id \
              WHERE ur.user_id = ? AND r.deleted_at IS NULL AND r.status = 1 \
                AND (ur.expire_time IS NULL OR ur.expire_time > now())",
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询发起人角色"))?;
    let roles: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get_typed::<String>("role_code").ok())
        .collect();
    if !roles
        .iter()
        .any(|code| code == crate::shared::constants::SUPER_ADMIN_ROLE_CODE)
    {
        return Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            "发起人当前没有任何角色含 SUPER_ADMIN：改状态机配置只能由超管落地".to_string(),
        ));
    }
    let username: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT username FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL LIMIT 1",
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询发起人账号"))?;
    let Some(username) = username else {
        return Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            "发起人账号已不存在 / 已停用：改状态机配置只能由在职超管落地".to_string(),
        ));
    };
    Ok(TokenClaims {
        jti: None,
        sub: user_id.to_string(),
        username: Some(username),
        roles,
        // 与登录一致：超管的权限码就是通配（`auth::model::load_permissions(super_admin = true)`）
        permissions: vec!["*:*:*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    })
}

// ==================== 内部工具 ====================

/// 当前用户 id（`401` 兜底）。
fn me_of(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 解析实体类型（未知 → 400 并列出全部合法值）。
fn parse_entity(raw: &str) -> std::result::Result<Entity, ApiError> {
    Entity::parse(raw.trim()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "未知的单据类型「{}」：状态机只认 {}",
                raw.trim(),
                Entity::ALL
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        )
    })
}

/// 按内核口径**试写一次再回滚**：把 400 / 403 提前到提交阶段。
async fn dry_run_upsert(
    claims: &TokenClaims,
    patch: &EdgeConfigPatch,
) -> std::result::Result<(), ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启校验事务"))?;
    let result = state_machine::upsert_edge_config(&tx, claims, patch).await;
    let _ = tx.rollback().await;
    result.map(|_| ())
}

/// 同上，删除路径（没配置时内核返回 `Ok(false)`，不是错误）。
async fn dry_run_delete(
    claims: &TokenClaims,
    entity: &Entity,
    from: &str,
    to: &str,
    confirm: bool,
) -> std::result::Result<(), ApiError> {
    if !confirm {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "改状态机配置需要二次确认（confirm = true）：这条边就是流程规则，\
             改它等于改全公司的审批路径"
                .to_string(),
        ));
    }
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启校验事务"))?;
    let result = state_machine::delete_edge_config(&tx, claims, entity.as_str(), from, to).await;
    let _ = tx.rollback().await;
    result.map(|_| ())
}

/// 某条边在跑的审批单号（409 报文用）。
async fn running_instance_no(biz_id: Uuid) -> std::result::Result<String, ApiError> {
    let value: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT instance_no FROM biz_flow_instance \
              WHERE biz_type = ? AND biz_id = ?::uuid AND status = 'PENDING' \
                AND deleted_at IS NULL ORDER BY started_at DESC LIMIT 1",
            vec![Value::Text(BIZ_TYPE.to_string()), Value::from(&biz_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询在跑的配置审批单"))?;
    Ok(value.unwrap_or_else(|| "(未知单号)".to_string()))
}

/// 审批单号（发起成功后回填响应）。
async fn instance_no_of(instance_id: Uuid) -> std::result::Result<String, ApiError> {
    let value: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询审批单号"))?;
    Ok(value.unwrap_or_default())
}

/// 在跑的配置变更审批单（管理端「这条边正在审批中」）。
async fn pending_changes(
    entity: Option<Entity>,
) -> std::result::Result<Vec<PendingChangeRes>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, instance_no, title, form_data, initiator_name, started_at \
               FROM biz_flow_instance \
              WHERE biz_type = ? AND status = 'PENDING' AND deleted_at IS NULL \
              ORDER BY started_at DESC",
            vec![Value::Text(BIZ_TYPE.to_string())],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询在跑的配置审批单"))?;
    let mut list = Vec::with_capacity(rows.len());
    for row in rows {
        let data: JsonValue = row.get_typed("form_data").unwrap_or(JsonValue::Null);
        let Some(change) = EdgeChange::from_form_data(&data) else {
            continue;
        };
        if entity.is_some_and(|want| want.as_str() != change.entity()) {
            continue;
        }
        list.push(PendingChangeRes {
            instance_id: row
                .get_typed::<Uuid>("id")
                .map(|value| value.to_string())
                .unwrap_or_default(),
            instance_no: row.get_typed("instance_no").unwrap_or_default(),
            title: row.get_typed("title").unwrap_or_default(),
            action: change.action().to_string(),
            entity: change.entity().to_string(),
            from: change.from().to_string(),
            to: change.to().to_string(),
            summary: change.summary(),
            initiator_name: row.get_typed("initiator_name").ok(),
            started_at: row
                .get_typed::<chrono::DateTime<Utc>>("started_at")
                .map(|value| value.to_rfc3339())
                .unwrap_or_default(),
        });
    }
    Ok(list)
}

/// `serde` 的 `bool` 默认值（`enabled` 缺省 = `true`）。
fn default_true() -> bool {
    true
}
