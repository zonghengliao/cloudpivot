//! 审批流程定义 · 接口层（批次 D · D1）。
//!
//! 依据：卡面 t_a92e9d5e「4. 定义侧 API（1–11）」+ `docs/批次D-工作流审批-技术架构.md` §10.1.1 / §10.4.3。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|------|------|--------|------|
//! | 1 | GET | `/business/flow/def/page` | `business:flow:read` | 定义分页（关键词 / 状态 / 业务类型） |
//! | 2 | GET | `/business/flow/def/{id}` | `business:flow:read` | 定义详情（含当前版本摘要） |
//! | 3 | POST | `/business/flow/def` | `business:flow:create` | 新建（同时建 v1 草稿 + START/END） |
//! | 4 | PUT | `/business/flow/def/{id}` | `business:flow:update` | 改元信息（不碰版本） |
//! | 5 | DELETE | `/business/flow/def/{id}` | `business:flow:delete` | 软删（有在跑审批单 → 409） |
//! | 6 | POST | `/business/flow/def/{id}/publish` | `business:flow:publish` | 发布（跑 R1–R15，写快照 / hash） |
//! | 7 | POST | `/business/flow/def/{id}/offline` | `business:flow:publish` | 停用（不改在跑实例） |
//! | 8 | GET | `/business/flow/def/{id}/versions` | `business:flow:read` | 版本列表 |
//! | 9 | GET | `/business/flow/version/{vid}` | `business:flow:read` | 版本详情 + 整图 |
//! | 10 | PUT | `/business/flow/version/{vid}/graph` | `business:flow:update` | 保存整图（只允许 DRAFT） |
//! | 11 | POST | `/business/flow/version/{vid}/validate` | `business:flow:update` | 只校验不保存 |
//! | 12 | POST | `/business/flow/def/{id}/draft` | `business:flow:update` | **另存为新草稿**（卡面「范围 3」要求） |
//!
//! 第 12 个接口不在卡面的 11 个里，是卡面「范围 · 3」明确要求的动作（「改流程 = 复制当前版本成 draft，
//! 改完再发布」）。没有它，「编辑已发布版本」只能靠前端猜版本号，违背架构 §3.6.3。
//!
//! # 三条红线在本层的落点
//!
//! 1. **PUBLISHED 版本的行只读**：[`save_graph`] 先查 `version_status`，非 `DRAFT` 直接
//!    `409 + 65022`，**任何**节点 / 边 / 审批人的写入都不发生在已发布版本上
//!    （不是「前端隐藏按钮」，是服务端硬拒）。
//! 2. **发布必须过 12 条校验**：[`publish`] 调 `graph.validate()`，非空即 `400`（错误码取第一条的码值，
//!    文案汇总前 5 条 + 「共 N 条」），**不会**带着不合法图进入 PUBLISHED。
//! 3. **改流程不改已发布版本**：[`create_draft`] 复制一份新 DRAFT，原 PUBLISHED 版本一行不动。
//!
//! # 保存 vs 发布的校验分工（架构 §10.4.3）
//!
//! `PUT graph` 只做**结构校验**（key 格式 / 唯一、from/to 存在、不重复边、没有自环、枚举取值）
//! 并把发布级问题放进 `warnings` 返回；`publish` 才跑全量 R1–R15。
//! 理由：编辑过程中图必然不完整（刚拖一个节点还没连线），保存期做发布校验等于让人没法存草稿。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::graph::{self, AssigneeKind, EdgeSpec, FlowGraph, Issue, NodeSpec, NodeType};
use crate::business::flow::task_bridge;
use crate::business::flow::{
    assignee, config_check, dto, instance_linkage, model, node_config, self_approval, task_node, template_vars,
};
use crate::business::notice::dto::ContentFormat;
use crate::business::notice::render;
use crate::shared::error_code;
use crate::utils;
use serde_json::{Map, Value};

// ==================== 小工具 ====================

/// 解析路径里的 UUID（非法 → `400 + 65005`，不静默当 404）。
fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| ApiError::new(400, error_code::FLOW_FIELD_INVALID, format!("{label}格式非法")))
}

/// 当前用户 id。
fn me_of(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 字段非法（`400 + 65005`）。
fn field_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::FLOW_FIELD_INVALID, message)
}

/// 解析查询串里的时间下界（RFC 3339；也认 `YYYY-MM-DD HH:MM:SS` 这种更短的手写格式）。
///
/// 首页工作台的「今日已办」把本地零点的 ISO 串放在 `since` 上传上来。
/// 解析失败必须报 400：静默当「没传」会让卡片把**全量历史已办**当成今日已办显示，
/// 数字看着正常、口径全错，比直接报错难查得多。
fn parse_since_field(raw: &str, label: &str) -> std::result::Result<chrono::DateTime<chrono::Utc>, ApiError> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw.trim()) {
        return Ok(parsed.with_timezone(&chrono::Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S") {
        return Ok(naive.and_utc());
    }
    Err(field_invalid(format!("{label} 时间格式非法（需要 RFC 3339）")))
}

/// P-FLOW-1B · `on_complete=APPROVE` 的 `approve_biz_type` 存在性校验（**发布 / 校验共用**）。
///
/// # 为什么不在 `graph::validate_with` 里
///
/// `FlowGraph::validate_with` 是**纯函数**：没有数据库句柄，60+ 条存量单测都靠这一点。
/// 「这个 `biz_type` 有没有已发布的默认流程」只能查库，所以放在 handler 这一层。
///
/// # 判据（与运行期同口径）
///
/// 调 `model::default_def_of_biz_type`：`deleted_at IS NULL`、`status = 'PUBLISHED'`、
/// `current_version_id IS NOT NULL`，按 `is_default` 优先取第一条。运行期
/// `task_bridge::start_task_node_approval` 用的是同一个函数；两处口径必须一样，
/// 否则会出现「发布时通过、跑起来 500」。
///
/// # 异常情况
///
/// 数据库异常 → [`ApiError`]（500）。
async fn approve_biz_type_issues(graph: &FlowGraph) -> std::result::Result<Vec<Issue>, ApiError> {
    let mut issues = Vec::new();
    for node in &graph.nodes {
        if node.node_type != NodeType::Task {
            continue;
        }
        // 形状问题（缺键 / 非法值）由 R18 的 `parse_task_config` 报，这里只管「存不存在」
        if node.config["task"]["on_complete"].as_str() != Some(node_config::OnComplete::Approve.as_str()) {
            continue;
        }
        let Some(biz_type) = node.config["task"]["approve_biz_type"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if model::default_def_of_biz_type(biz_type)
            .await
            .map_err(|error| utils::db_error(error, "查询审批流定义"))?
            .is_some()
        {
            continue;
        }
        issues.push(
            Issue::new(
                "R18",
                error_code::FLOW_TASK_NODE_INVALID,
                format!(
                    "任务节点 {}（{}）的 on_complete=APPROVE 指定了审批流 {biz_type}，\
                     但该业务类型下没有已发布的默认流程；请先发布一个 {biz_type} 流程并设为默认",
                    node.key, node.name
                ),
            )
            .with_nodes(vec![node.key.clone()]),
        );
    }
    Ok(issues)
}

/// 校验不通过汇总成 `ApiError`：**错误码取第一条**（前端按码值映射文案），
/// 消息汇总前 5 条，多了给「共 N 条」（全部条目写日志）。
fn validation_error(issues: &[Issue]) -> ApiError {
    for issue in issues {
        tracing::warn!(rule = issue.rule, code = issue.code, message = %issue.message, "流程发布校验不通过");
    }
    let mut message = issues
        .iter()
        .take(5)
        .map(|issue| issue.message.clone())
        .collect::<Vec<_>>()
        .join("；");
    if issues.len() > 5 {
        message.push_str(&format!("（共 {} 条问题，请逐条修正）", issues.len()));
    }
    let code = issues
        .first()
        .map(|issue| issue.code)
        .unwrap_or(error_code::FLOW_GRAPH_STRUCTURE);
    ApiError::new(400, code, message)
}

// ==================== 行 → 出参 ====================

fn def_res(row: &model::DefRow) -> dto::DefRes {
    dto::DefRes {
        id: row.id.to_string(),
        flow_code: row.flow_code.clone(),
        flow_name: row.flow_name.clone(),
        biz_type: row.biz_type.clone(),
        category: row.category.clone().unwrap_or_default(),
        description: row.description.clone().unwrap_or_default(),
        status: row.status.clone(),
        current_version_id: row.current_version_id.map(|id| id.to_string()),
        current_version_no: row.current_version_no,
        is_default: row.is_default,
        allow_auto_approve: row.allow_auto_approve,
        node_count: row.cur_node_count,
        edge_count: row.cur_edge_count,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn version_summary(row: &model::VersionRow) -> dto::VersionSummaryRes {
    dto::VersionSummaryRes {
        id: row.id.to_string(),
        version_no: row.version_no,
        version_status: row.version_status.clone(),
        is_current: row.is_current,
        node_count: row.node_count,
        edge_count: row.edge_count,
        graph_hash: row.graph_hash.clone(),
        change_note: row.change_note.clone(),
        published_at: row.published_at,
        published_by: row.published_by.map(|id| id.to_string()),
        published_by_name: row.published_by_name.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn node_res(node: &NodeSpec) -> dto::GraphNodeRes {
    dto::GraphNodeRes {
        key: node.key.clone(),
        name: node.name.clone(),
        node_type: node.node_type.as_str().to_string(),
        x: node.x,
        y: node.y,
        sort_order: node.sort_order,
        assignee_type: node.assignee_type.map(|kind| kind.as_str().to_string()),
        superior_levels: node.superior_levels,
        form_var_key: node.form_var_key.clone(),
        approval_mode: node.approval_mode.map(|mode| mode.as_str().to_string()),
        approve_ratio: node.approve_ratio,
        reject_veto: node.reject_veto,
        empty_assignee: node.empty_assignee.clone(),
        self_approval: node.self_approval.clone(),
        allow_robot: node.allow_robot,
        fallback_user_id: node.fallback_user_id.map(|id| id.to_string()),
        fallback_role_id: node.fallback_role_id.map(|id| id.to_string()),
        join_mode: node.join_mode.as_str().to_string(),
        fork_mode: node.fork_mode.as_str().to_string(),
        timeout_hours: node.timeout_hours,
        timeout_action: node.timeout_action.clone(),
        remind_before_hours: node.remind_before_hours,
        max_remind_times: node.max_remind_times,
        cc_on_approved: node.cc_on_approved,
        config: node.config.clone(),
        subprocess_def_id: node.subprocess_def_id.map(|id| id.to_string()),
        assignees: node
            .assignees
            .iter()
            .map(|assignee| dto::AssigneeRes {
                kind: assignee.kind.clone(),
                ref_id: assignee.ref_id.clone(),
                sort_order: assignee.sort_order,
            })
            .collect(),
    }
}

fn edge_res(edge: &EdgeSpec) -> dto::GraphEdgeRes {
    dto::GraphEdgeRes {
        key: edge.key.clone(),
        from: edge.from.clone(),
        to: edge.to.clone(),
        condition: edge.condition.clone(),
        label: edge.label.clone(),
        priority: edge.priority,
        is_default: edge.is_default,
    }
}

/// 新建流程时的默认图：START → END（一条边）。
///
/// 为什么给一条边而不是两个孤立节点：让「新建 → 发布」这条最短路径立刻可走通
/// （验收会用它验证发布链路），同时编辑器打开就是一张能跑的最简图，而不是两个漂浮的点。
fn default_graph() -> FlowGraph {
    FlowGraph {
        nodes: vec![
            NodeSpec::new("n_start", "开始", NodeType::Start),
            NodeSpec::new("n_end", "结束", NodeType::End),
        ],
        edges: vec![EdgeSpec::new("e_start_end", "n_start", "n_end")],
    }
}

/// 取流程定义（不存在 → `404 + 65001`）。
async fn load_def(id: Uuid) -> std::result::Result<model::DefRow, ApiError> {
    model::find_def(id)
        .await
        .map_err(|error| utils::db_error(error, "查询流程定义"))?
        .ok_or_else(model::not_found)
}

/// 取版本（不存在 → `404`）。
async fn load_version(vid: Uuid) -> std::result::Result<model::VersionRow, ApiError> {
    model::find_version(vid)
        .await
        .map_err(|error| utils::db_error(error, "查询流程版本"))?
        .ok_or_else(model::version_not_found)
}

// ==================== 1. 定义分页 ====================

/// 流程定义分页（`GET /business/flow/def/page`）。
///
/// # 参数
///
/// - `keyword`：编码**或**名称的关键词（ILIKE，通配符已转义）；
/// - `status`：`DRAFT` / `PUBLISHED` / `DISABLED`（非法值 → 400，不静默忽略）；
/// - `biz_type`：业务类型（大小写不敏感）；
/// - `page` / `page_size`：框架 `normalize_page` 归一化。
///
/// # 异常情况
///
/// - 400 + `65005`：`status` 取值非法；
/// - 500：数据库异常。
#[nik::permission(path = "/business/flow/def/page", method = "GET", permission = "business:flow:read")]
#[nik::get("/business/flow/def/page")]
pub async fn page(Query(query): Query<dto::DefPageQuery>) -> ResResult<dto::DefPageRes> {
    let status = match query.status.as_deref() {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(dto::normalize_def_status(raw).ok_or_else(|| {
            ApiError::new(
                400,
                error_code::FLOW_FIELD_INVALID,
                format!("状态 {raw} 非法（只允许 DRAFT / PUBLISHED / DISABLED）"),
            )
        })?),
    };
    let biz_type = match query.biz_type.as_deref() {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(dto::normalize_biz_type(raw).map_err(field_invalid)?),
    };
    let filter = model::DefFilter {
        keyword: query.normalized_keyword(),
        status,
        biz_type,
    };
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_defs(&filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询流程定义"))?;
    Ok(Res::ok(dto::DefPageRes {
        list: result.list.iter().map(def_res).collect(),
        total: result.total,
        page: result.page,
        page_size: result.size,
    }))
}

// ==================== 2. 定义详情 ====================

/// 流程定义详情（`GET /business/flow/def/{id}`）。
///
/// 含当前发布版本的 `node_count` / `edge_count` / `version_no`，前端不用再拉一次版本列表。
///
/// # 异常情况
///
/// - 400 + `65005`：id 不是 UUID；
/// - 404 + `65001`：流程不存在 / 已软删。
#[nik::permission(path = "/business/flow/def/{id}", method = "GET", permission = "business:flow:read")]
#[nik::get("/business/flow/def/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<dto::DefRes> {
    let def_id = parse_uuid_field(&id, "流程 id")?;
    let row = load_def(def_id).await?;
    Ok(Res::ok(def_res(&row)))
}

// ==================== 3. 新建 ====================

/// 新建流程（`POST /business/flow/def`）。
///
/// 同时建 **v1 DRAFT 版本** + 两个默认节点（START / END）+ 一条边（架构 §10.1.1 #3）。
///
/// # 异常情况
///
/// - 400 + `65005`：编码 / 名称 / 业务类型非法；
/// - 409 + `10003`：编码已被占用（未软删唯一）；
/// - 500：数据库异常。
#[nik::permission(path = "/business/flow/def", method = "POST", permission = "business:flow:create")]
#[nik::post("/business/flow/def")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::DefCreateReq>,
) -> ResResult<dto::DefSaveRes> {
    let operator = me_of(&claims)?;
    let flow_code = dto::normalize_flow_code(&req.flow_code).map_err(field_invalid)?;
    let flow_name = dto::normalize_name(&req.flow_name).map_err(field_invalid)?;
    let biz_type = dto::normalize_biz_type(&req.biz_type).map_err(field_invalid)?;
    let category = dto::normalize_category(req.category.as_deref()).map_err(field_invalid)?;
    let description = dto::normalize_description(req.description.as_deref()).map_err(field_invalid)?;

    if model::flow_code_taken(&flow_code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验流程编码"))?
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("流程编码 {flow_code} 已被占用"),
        ));
    }

    let new_def = model::NewDef {
        id: Uuid::now_v7(),
        flow_code: flow_code.clone(),
        flow_name,
        biz_type: biz_type.clone(),
        category,
        description,
        is_default: req.is_default.unwrap_or(false),
        allow_auto_approve: req.allow_auto_approve.unwrap_or(false),
    };
    let graph = default_graph();
    let node_count = graph.nodes.len() as i64;
    let edge_count = graph.edges.len() as i64;

    let def_id = new_def.id;
    let biz_type_for_default = new_def.biz_type.clone();
    let wants_default = new_def.is_default;
    let version_id = db()
        .transaction(|tx| async move {
            if wants_default {
                // 同一 biz_type 至多一个默认流程（uk_biz_flow_def_default）
                model::clear_default_of_biz_type(&tx, &biz_type_for_default, def_id).await?;
            }
            model::create_def(&tx, &new_def, operator).await?;
            let version_id = model::create_version(&tx, def_id, 1, Some("初始版本"), operator).await?;
            model::replace_graph(&tx, version_id, &graph).await?;
            model::touch_version(&tx, version_id, Some("初始版本"), node_count, edge_count, operator).await?;
            Ok(version_id)
        })
        .await
        .map_err(|error| utils::db_error(error, "新建流程定义"))?;

    tracing::info!(%def_id, %version_id, flow_code = %flow_code, operator = %operator, "流程定义已创建");
    Ok(Res::ok(dto::DefSaveRes {
        id: def_id.to_string(),
        version_id: version_id.to_string(),
        version_no: 1,
        node_count,
        edge_count,
    }))
}

// ==================== 4. 改元信息 ====================

/// 改流程元信息（`PUT /business/flow/def/{id}`）。
///
/// **只动 `biz_flow_def`，不碰任何版本行**（版本内容只能通过图接口改，且只允许 DRAFT）。
///
/// # 异常情况
///
/// - 400 + `65005`：字段非法 / 一个字段都没传；
/// - 404 + `65001`：流程不存在；
/// - 409 + `10003`：把 `is_default` 置真时与同业务类型的其它默认流程冲突（已被自动清理，仍冲突才会报）。
#[nik::permission(
    path = "/business/flow/def/{id}",
    method = "PUT",
    permission = "business:flow:update"
)]
#[nik::put("/business/flow/def/{id}")]
pub async fn update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::DefUpdateReq>,
) -> ResResult<dto::DefRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    if req.is_empty() {
        return Err(field_invalid("没有要修改的字段"));
    }
    load_def(def_id).await?;

    let mut patch = model::DefPatch::default();
    if let Some(name) = req.flow_name.as_deref() {
        patch.flow_name = Some(dto::normalize_name(name).map_err(field_invalid)?);
    }
    if let Some(biz_type) = req.biz_type.as_deref() {
        patch.biz_type = Some(dto::normalize_biz_type(biz_type).map_err(field_invalid)?);
    }
    if let Some(category) = req.category.as_ref() {
        patch.category = Some(dto::normalize_category(category.as_deref()).map_err(field_invalid)?);
    }
    if let Some(description) = req.description.as_ref() {
        patch.description = Some(dto::normalize_description(description.as_deref()).map_err(field_invalid)?);
    }
    patch.is_default = req.is_default;
    patch.allow_auto_approve = req.allow_auto_approve;

    let biz_type_for_default = patch.biz_type.clone();
    let is_default_enabled = patch.is_default == Some(true);
    db().transaction(|tx| async move {
        if is_default_enabled {
            // 同一 biz_type 至多一个默认流程（部分唯一索引 uk_biz_flow_def_default）
            let biz_type = match biz_type_for_default {
                Some(value) => Some(value),
                None => model::biz_type_of(&tx, def_id).await?,
            };
            if let Some(biz_type) = biz_type {
                model::clear_default_of_biz_type(&tx, &biz_type, def_id).await?;
            }
        }
        model::update_def(def_id, &patch, operator).await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "更新流程定义"))?;

    let row = load_def(def_id).await?;
    Ok(Res::ok(def_res(&row)))
}

// ==================== 5. 软删 ====================

/// 删除流程（`DELETE /business/flow/def/{id}`）。
///
/// 软删：`deleted_at` 置时间，版本 / 节点 / 边 / 审批人**一行不删**
/// （历史实例仍绑着它们的 `version_id`，架构 D2）。软删后编码可被新流程复用。
///
/// # 异常情况
///
/// - 404 + `65001`：流程不存在；
/// - 409 + `65021`：还有进行中的审批单（D2 落地后生效）。
#[nik::permission(
    path = "/business/flow/def/{id}",
    method = "DELETE",
    permission = "business:flow:delete"
)]
#[nik::delete("/business/flow/def/{id}")]
pub async fn remove(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::DefDeleteRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    let def = load_def(def_id).await?;

    if model::has_running_instance(def_id)
        .await
        .map_err(|error| utils::db_error(error, "检查在跑审批单"))?
    {
        return Err(ApiError::new(
            409,
            error_code::FLOW_DEF_IN_USE,
            "该流程还有进行中的审批单，不能删除（请先处理完或停用）",
        ));
    }

    model::soft_delete_def(def_id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除流程定义"))?;
    tracing::info!(%def_id, flow_code = %def.flow_code, operator = %operator, "流程定义已软删");
    Ok(Res::ok(dto::DefDeleteRes {
        deleted: true,
        flow_code: def.flow_code,
    }))
}

// ==================== 6. 发布 ====================

/// 发布流程（`POST /business/flow/def/{id}/publish`）。
///
/// 步骤（顺序不能换，架构 §3.6 / §6.1）：
///
/// 1. 取**最新 DRAFT 版本**（没有 → 409「没有可发布的草稿版本」）；
/// 2. 读整图（3 条 SELECT）；
/// 3. 跑 **R1–R15**，任一不过 → 400（错误码取第一条的码值）；
/// 4. `graph_snapshot`（canonical JSON）+ `graph_hash`（sha256）；
/// 5. 一个事务里：旧 PUBLISHED → ARCHIVED，本版本 → PUBLISHED + is_current，
///    `def.current_version_id` / `current_version_no` / `status='PUBLISHED'`。
///
/// # 异常情况
///
/// - 404 + `65001`：流程 / 版本不存在；
/// - 400 + `65010`–`65017`：12 条校验不通过（文案点名节点 / 边）；
/// - 409 + `65022`：没有可发布的草稿版本。
#[nik::permission(
    path = "/business/flow/def/{id}/publish",
    method = "POST",
    permission = "business:flow:publish"
)]
#[nik::post("/business/flow/def/{id}/publish")]
pub async fn publish(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::PublishRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    load_def(def_id).await?;

    let draft = model::latest_draft(def_id)
        .await
        .map_err(|error| utils::db_error(error, "查询草稿版本"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::FLOW_VERSION_NOT_EDITABLE,
                "没有可发布的草稿版本（已发布的流程要改请先「另存为新草稿」）",
            )
        })?;

    let graph = model::load_graph(draft.id)
        .await
        .map_err(|error| utils::db_error(error, "读取流程整图"))?;

    // 空图 = 没配过，先给一个能读懂的错误（R1 会报「0 个开始节点」，但文案不如这句直白）
    if graph.nodes.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::FLOW_GRAPH_STRUCTURE,
            "流程还没有任何节点，不能发布",
        ));
    }

    let mut issues = graph.validate();
    // P-FLOW-1B：APPROVE 的 `approve_biz_type` 必须存在（发布期就必须拦住 ——
    // 否则配置能发布出去，运行期任务完成时才 500）。
    issues.extend(approve_biz_type_issues(&graph).await?);
    // P-FLOW-2：通知节点引用的 `template_code` 必须存在且启用（同上，纯函数拿不到 DB）。
    issues.extend(
        crate::business::flow::notify_node::template_issues(&graph)
            .await
            .map_err(|error| utils::db_error(error, "校验通知模板"))?,
    );
    // P3-08（ARCH-5 §4.14）：SUBPROCESS 节点指向的子流程定义必须存在 / 已发布，
    // 且不许自递归、嵌套深度 ≤ 3（这两条要顺着"定义 → 当前版本 → 它的子流程"查库）。
    issues.extend(
        crate::business::flow::subprocess::validate_publish(&graph, def_id)
            .await
            .map_err(|error| utils::db_error(error, "校验子流程定义"))?,
    );
    // N-7 ②：发布前的**全流程预演** —— 兜底人 / 兜底角色 / 升级链 / 每个节点的可枚举审批人
    // 必须现在就能解析。发布是把这版图冻结成快照给所有新单用，此刻配错等于每张新单都会踩。
    let lookup = config_check::load(db(), &graph)
        .await
        .map_err(|error| utils::db_error(error, "校验审批人可解析性"))?;
    issues.extend(config_check::issues(&graph, &lookup, config_check::Phase::Publish));
    if !issues.is_empty() {
        return Err(validation_error(&issues));
    }

    let snapshot = graph.snapshot_json();
    let graph_hash = graph.graph_hash();
    let node_count = graph.nodes.len() as i64;
    let edge_count = graph.edges.len() as i64;
    let version_no = draft.version_no;
    let version_id = draft.id;

    let snapshot_for_tx = snapshot.clone();
    let hash_for_tx = graph_hash.clone();
    db().transaction(|tx| async move {
        model::apply_publish(
            &tx,
            &model::PublishWrite {
                def_id,
                version_id,
                version_no,
                snapshot: &snapshot_for_tx,
                graph_hash: &hash_for_tx,
                node_count,
                edge_count,
                operator,
            },
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "发布流程定义"))?;

    let published = load_version(version_id).await?;
    tracing::info!(
        %def_id,
        %version_id,
        version_no,
        graph_hash = %graph_hash,
        node_count,
        edge_count,
        operator = %operator,
        "流程定义已发布"
    );
    Ok(Res::ok(dto::PublishRes {
        def_id: def_id.to_string(),
        version_id: version_id.to_string(),
        version_no,
        status: dto::DEF_STATUS_PUBLISHED.to_string(),
        graph_hash,
        node_count,
        edge_count,
        published_at: published.published_at.unwrap_or_else(chrono::Utc::now),
    }))
}

// ==================== 7. 停用 ====================

/// 停用流程（`POST /business/flow/def/{id}/offline`）。
///
/// 只把 `def.status` 改成 `DISABLED`：**不动版本、不动在跑实例**（架构 D2 / D9）。
/// 停用后不能再发起新实例（D2 的发起接口据此判定），已发出的单子照原版本跑完。
///
/// # 异常情况
///
/// - 404 + `65001`：流程不存在。
#[nik::permission(
    path = "/business/flow/def/{id}/offline",
    method = "POST",
    permission = "business:flow:publish"
)]
#[nik::post("/business/flow/def/{id}/offline")]
pub async fn offline(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::OfflineRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    load_def(def_id).await?;
    model::mark_disabled(def_id, operator)
        .await
        .map_err(|error| utils::db_error(error, "停用流程定义"))?;
    let row = load_def(def_id).await?;
    tracing::info!(%def_id, operator = %operator, "流程定义已停用");
    Ok(Res::ok(dto::OfflineRes {
        id: def_id.to_string(),
        status: row.status,
        current_version_no: row.current_version_no,
    }))
}

// ==================== 7b. 恢复启用 ====================

/// 恢复启用流程（`POST /business/flow/def/{id}/enable`）。
///
/// 「停用」的反向操作，只做 **`DISABLED → PUBLISHED`** 一件事：
/// 不动版本、不动在跑实例、不动 `is_default`。
///
/// # 为什么不能复用 `publish`
///
/// `publish` 的语义是「把草稿校验成快照」：它要求存在 DRAFT 版本，并且要跑完 12 条发布
/// 校验。而一个被停用的流程**没有草稿**（`current_version_id` 指向已发布版本），
/// 用它启用会稳定返回 `409 没有可发布的草稿版本` —— 模板库点「启用」就永远失败。
/// 所以「恢复启用」必须是独立入口：它不产生新版本，只把状态位从 `DISABLED` 拨回 `PUBLISHED`。
///
/// 反过来，DRAFT 流程不能走这里（[`model::mark_enabled`] 的 `WHERE status = 'DISABLED'`
/// 会返回 0 行 → 409），否则等于绕过发布校验直接把未校验的图放上线。
///
/// # 异常情况
///
/// - 404 + `65001`：流程不存在 / 已软删；
/// - 409 + `65006`：当前状态不是 `DISABLED`（草稿请走发布接口）；
/// - 409 + `65006`：该流程从来没有发布过版本（没有可运行的图）。
#[nik::permission(
    path = "/business/flow/def/{id}/enable",
    method = "POST",
    permission = "business:flow:publish"
)]
#[nik::post("/business/flow/def/{id}/enable")]
pub async fn enable(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::EnableRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    let before = load_def(def_id).await?;

    if before.status != dto::DEF_STATUS_DISABLED {
        return Err(ApiError::new(
            409,
            error_code::FLOW_VERSION_NOT_EDITABLE,
            format!(
                "只有已停用的流程能恢复启用（当前状态 {}）。草稿请用「发布」，启用中的流程无需操作",
                before.status
            ),
        ));
    }
    if before.current_version_id.is_none() {
        return Err(ApiError::new(
            409,
            error_code::FLOW_VERSION_NOT_EDITABLE,
            "该流程还没有发布过版本，无法直接启用：请先在流程编辑器里发布草稿",
        ));
    }

    let affected = model::mark_enabled(def_id, operator)
        .await
        .map_err(|error| utils::db_error(error, "恢复启用流程定义"))?;
    if affected == 0 {
        // 并发：读到的状态与写入时不一致（别人刚改过），如实报冲突而不是假装成功
        return Err(ApiError::new(
            409,
            error_code::FLOW_VERSION_NOT_EDITABLE,
            "流程状态已被其他操作改动，请刷新后重试",
        ));
    }

    let row = load_def(def_id).await?;
    tracing::info!(%def_id, operator = %operator, status = %row.status, "流程定义已恢复启用");
    Ok(Res::ok(dto::EnableRes {
        id: def_id.to_string(),
        status: row.status,
        current_version_no: row.current_version_no,
    }))
}

// ==================== 8. 版本列表 ====================
/// 版本列表（`GET /business/flow/def/{id}/versions`）。
///
/// 版本号倒序；含发布人姓名 / 时间 / 节点数 / 当前标记。
///
/// # 异常情况
///
/// - 404 + `65001`：流程不存在。
#[nik::permission(
    path = "/business/flow/def/{id}/versions",
    method = "GET",
    permission = "business:flow:read"
)]
#[nik::get("/business/flow/def/{id}/versions")]
pub async fn versions(Path(id): Path<String>) -> ResResult<dto::VersionListRes> {
    let def_id = parse_uuid_field(&id, "流程 id")?;
    load_def(def_id).await?;
    let rows = model::versions_of(def_id)
        .await
        .map_err(|error| utils::db_error(error, "查询流程版本"))?;
    Ok(Res::ok(dto::VersionListRes {
        list: rows.iter().map(version_summary).collect(),
    }))
}

// ==================== 9. 版本详情 ====================

/// 版本详情 + 整图（`GET /business/flow/version/{vid}`）。
///
/// 返回库里的**编辑期行**（不是快照）：编辑器要靠行做增量修改；
/// `has_snapshot` 告诉前端这个版本是否已发布（已发布时行与快照内容一致，只读）。
///
/// # 异常情况
///
/// - 400 + `65005`：vid 不是 UUID；
/// - 404 + `65001`：版本不存在。
#[nik::permission(
    path = "/business/flow/version/{vid}",
    method = "GET",
    permission = "business:flow:read"
)]
#[nik::get("/business/flow/version/{vid}")]
pub async fn version_detail(Path(vid): Path<String>) -> ResResult<dto::VersionRes> {
    let version_id = parse_uuid_field(&vid, "版本 id")?;
    let version = load_version(version_id).await?;
    let def = load_def(version.def_id).await?;
    let graph = model::load_graph(version_id)
        .await
        .map_err(|error| utils::db_error(error, "读取流程整图"))?;
    let snapshot = model::load_snapshot(version_id)
        .await
        .map_err(|error| utils::db_error(error, "读取流程图快照"))?;
    Ok(Res::ok(dto::VersionRes {
        summary: version_summary(&version),
        def_id: version.def_id.to_string(),
        flow_code: def.flow_code,
        flow_name: def.flow_name,
        editable: version.editable(),
        has_snapshot: snapshot.is_some(),
        nodes: graph.nodes.iter().map(node_res).collect(),
        edges: graph.edges.iter().map(edge_res).collect(),
    }))
}

// ==================== 10. 保存整图 ====================

/// 保存整图（`PUT /business/flow/version/{vid}/graph`）。
///
/// **只允许 DRAFT 版本**（红线：PUBLISHED 的行只读）：非 DRAFT → `409 + 65022`，
/// 文案直接教用户走「另存为新草稿」。
///
/// 整图覆盖式保存：删掉该版本全部节点（级联删边与审批人）再整份插入，
/// 一个事务完成（架构 §3.1：编辑器读行、写行，不做逐节点 patch）。
///
/// # 异常情况
///
/// - 400 + `65005`：结构非法（key 重复 / from-to 不存在 / 自环 / 重复边 / 枚举非法）；
/// - 404 + `65001`：版本不存在；
/// - 409 + `65022`：版本已发布，不能直接编辑。
#[nik::permission(
    path = "/business/flow/version/{vid}/graph",
    method = "PUT",
    permission = "business:flow:update"
)]
#[nik::put("/business/flow/version/{vid}/graph")]
pub async fn save_graph(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(vid): Path<String>,
    Json(req): Json<dto::GraphSaveReq>,
) -> ResResult<dto::GraphSaveRes> {
    let operator = me_of(&claims)?;
    let version_id = parse_uuid_field(&vid, "版本 id")?;
    let version = load_version(version_id).await?;
    if !version.editable() {
        return Err(ApiError::new(
            409,
            error_code::FLOW_VERSION_NOT_EDITABLE,
            "该版本已发布，不能直接编辑（请先「另存为新草稿」再改）",
        ));
    }

    let graph = req.into_graph().map_err(field_invalid)?;
    // 保存期：表达式审批人的**结构**校验（400 + `65016`，P1-A2 / EXPRESSION-DSL §8.5）。
    //
    // 必须在这里拦：库侧 `ck_biz_flow_node_expression` 只兜底「有没有 expression」，
    // 结构错交给库的话用户拿到的是 500「服务器内部错误」（没有一句中文原因）。
    // 发布期还有 R16 做双保险（并额外拒 `PREV_APPROVER` → `65033`，D-E8）。
    dto::validate_expression_nodes(&graph)?;
    // N-7 ②：兜底人 / 兜底角色 / 角色升级链是**最后一道保险**，配错了等于没有保险。
    // 它必须当场拦住（400 + 点名节点），而不是等有人发起、走到该节点才 422 65042/65040。
    // 普通节点的指定审批人**不在保存期拦**：编辑器是边改边存的，允许先落半成品；
    // 这些进 `warnings`（见下），发布期才阻断（`Phase::Publish` 的全流程预演）。
    let lookup = config_check::load(db(), &graph)
        .await
        .map_err(|error| utils::db_error(error, "校验节点兜底人与升级链"))?;
    let blocking = config_check::issues(&graph, &lookup, config_check::Phase::Save);
    if !blocking.is_empty() {
        return Err(validation_error(&blocking));
    }
    let change_note = req.normalized_change_note();
    let node_count = graph.nodes.len() as i64;
    let edge_count = graph.edges.len() as i64;

    // 保存期只提示、不阻断（架构 §10.4.3）；预演类问题（R32）也进提示，
    // 让编辑者保存时就看到「这个审批人已经没了」，不必等到发布被拒。
    let mut warnings: Vec<String> = graph
        .validate()
        .iter()
        .map(|issue| format!("[{}] {}", issue.rule, issue.message))
        .collect();
    warnings.extend(
        config_check::issues(&graph, &lookup, config_check::Phase::Publish)
            .iter()
            .map(|issue| format!("[{}] {}", issue.rule, issue.message)),
    );

    let note_for_write = change_note.clone();
    db().transaction(|tx| async move {
        model::replace_graph(&tx, version_id, &graph).await?;
        model::touch_version(
            &tx,
            version_id,
            note_for_write.as_deref(),
            node_count,
            edge_count,
            operator,
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "保存流程整图"))?;

    tracing::info!(
        %version_id,
        version_no = version.version_no,
        node_count,
        edge_count,
        warnings = warnings.len(),
        operator = %operator,
        "流程整图已保存"
    );
    Ok(Res::ok(dto::GraphSaveRes {
        version_id: version_id.to_string(),
        version_no: version.version_no,
        node_count,
        edge_count,
        warnings,
    }))
}

// ==================== 11. 只校验 ====================

/// 只校验不保存（`POST /business/flow/version/{vid}/validate`）。
///
/// 返回 **R1–R15 的全部不通过项**（不是第一个）：前端编辑器要把所有问题一次性标在图上。
/// 这是一个 **200 响应**（校验本身成功执行），不通过与否看 `passed` 字段。
///
/// # 异常情况
///
/// - 404 + `65001`：版本不存在。
#[nik::permission(
    path = "/business/flow/version/{vid}/validate",
    method = "POST",
    permission = "business:flow:update"
)]
#[nik::post("/business/flow/version/{vid}/validate")]
pub async fn validate(Path(vid): Path<String>) -> ResResult<dto::ValidateRes> {
    let version_id = parse_uuid_field(&vid, "版本 id")?;
    let version = load_version(version_id).await?;
    let graph = model::load_graph(version_id)
        .await
        .map_err(|error| utils::db_error(error, "读取流程整图"))?;
    let mut issues = graph.validate();
    // P-FLOW-1B：`on_complete=APPROVE` 的 `approve_biz_type` 必须指向已发布的默认流程。
    // 纯函数的 `validate_with` 查不了库，这条放在这里（发布 / 只校验两个入口共用）。
    issues.extend(approve_biz_type_issues(&graph).await?);
    // N-7 ②：只校验 = 一次「全流程预演」，与 publish 用同一份判据（Phase::Publish），
    // 这样编辑器点「校验」看到的就是发布时会拦的全部问题。
    let lookup = config_check::load(db(), &graph)
        .await
        .map_err(|error| utils::db_error(error, "校验审批人可解析性"))?;
    issues.extend(config_check::issues(&graph, &lookup, config_check::Phase::Publish));
    Ok(Res::ok(dto::ValidateRes {
        version_id: version_id.to_string(),
        version_no: version.version_no,
        passed: issues.is_empty(),
        issues: issues.iter().map(dto::IssueRes::from).collect(),
        node_count: graph.nodes.len() as i64,
        edge_count: graph.edges.len() as i64,
    }))
}

// ==================== 12. 另存为新草稿 ====================

/// 另存为新草稿（`POST /business/flow/def/{id}/draft`）。
///
/// 「编辑已发布版本」的**唯一正确姿势**（架构 §3.6.3）：复制一份 `version_no = max + 1` 的
/// 新 DRAFT，原 PUBLISHED 版本一行不动。改完再走 publish。
///
/// 复制源：显式 `source_version_id`；否则 `def.current_version_id`；再否则最新版本。
///
/// # 异常情况
///
/// - 400 + `65005`：`source_version_id` 非法 / 不属于该流程；
/// - 404 + `65001`：流程 / 版本不存在；
/// - 409 + `10003`：目标版本号已被占用（并发下另一张卡先建了草稿，重试即可）。
#[nik::permission(
    path = "/business/flow/def/{id}/draft",
    method = "POST",
    permission = "business:flow:update"
)]
#[nik::post("/business/flow/def/{id}/draft")]
pub async fn create_draft(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::DraftReq>,
) -> ResResult<dto::DraftRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    let def = load_def(def_id).await?;

    // 复制源：显式 > 当前发布版 > 最新版本
    let source = match req.source_version_id.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            let source_id = parse_uuid_field(raw, "来源版本 id")?;
            let version = load_version(source_id).await?;
            if version.def_id != def_id {
                return Err(field_invalid("来源版本不属于该流程"));
            }
            version
        }
        _ => match def.current_version_id {
            Some(current_id) => load_version(current_id).await?,
            None => {
                let rows = model::versions_of(def_id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询流程版本"))?;
                rows.into_iter().next().ok_or_else(|| {
                    ApiError::new(
                        409,
                        error_code::FLOW_VERSION_NOT_EDITABLE,
                        "该流程还没有任何版本，无法另存草稿",
                    )
                })?
            }
        },
    };

    let change_note = req
        .change_note
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(dto::MAX_CHANGE_NOTE_CHARS).collect::<String>());

    let source_no = source.version_no;
    let source_id = source.id;
    let note_for_write = change_note.clone();
    let (new_version_id, node_count, edge_count) = db()
        .transaction(|tx| async move {
            let next_no = model::max_version_no(&tx, def_id).await? + 1;
            let new_version_id =
                model::create_version(&tx, def_id, next_no, note_for_write.as_deref(), operator).await?;
            let (node_count, edge_count) = model::copy_version_to_draft(
                &tx,
                def_id,
                source_id,
                new_version_id,
                note_for_write.as_deref(),
                operator,
            )
            .await?;
            Ok((new_version_id, node_count, edge_count))
        })
        .await
        .map_err(|error| utils::db_error(error, "另存为新草稿"))?;

    let created = load_version(new_version_id).await?;
    tracing::info!(
        %def_id,
        %new_version_id,
        version_no = created.version_no,
        source_version_no = source_no,
        node_count,
        edge_count,
        operator = %operator,
        "已另存为新草稿"
    );
    Ok(Res::ok(dto::DraftRes {
        def_id: def_id.to_string(),
        version_id: new_version_id.to_string(),
        version_no: created.version_no,
        source_version_no: source_no,
        node_count,
        edge_count,
    }))
}

// ==================== 运行侧：实例 / 待办 / 抄送（12–23，批次 D · D2） ====================
//
// 依据：卡面 t_3715c446「4. 实例侧 API（12–23）」+ 架构 §10.1.2 / §10.1.3 / §10.1.4。
//
// | # | 方法 | 路径 | 权限码 |
// |---|------|------|--------|
// | 12 | POST | `/business/flow/instance` | `business:flow:instance:create` |
// | 13 | GET | `/business/flow/instance/page` | `business:flow:instance:read` |
// | 14 | GET | `/business/flow/instance/{id}` | `business:flow:instance:read` |
// | 14b | GET | `/business/flow/instance/by-biz` | `business:flow:instance:read` |（按业务单据 `biz_id`/`biz_no` 反查审批单 + 时间线，VTRACE-5 全链路视图）
// | 15 | POST | `/business/flow/instance/{id}/withdraw` | `business:flow:instance:withdraw` |
// | 15b | POST | `/business/flow/instance/{id}/terminate` | `business:flow:admin` |（管理员人工终止，E2E01-FIX-3）
// | 16 | POST | `/business/flow/task/{id}/approve` | `business:flow:approve` |
// | 17 | POST | `/business/flow/task/{id}/reject` | `business:flow:approve` |
// | 18 | POST | `/business/flow/task/{id}/transfer` | `business:flow:approve` |
// | 19 | POST | `/business/flow/task/{id}/add-sign` | `business:flow:approve` |
// | 20 | POST | `/business/flow/task/batch` | `business:flow:approve` |（兼容层：逐条委托 `/task/batch-approve`，语义与单条完全一致）
// | 20b | POST | `/business/flow/task/batch-approve` | `business:flow:approve` |（`flow::batch`，D7 之后的唯一实现）
// | 21 | GET | `/business/flow/task/todo` | `business:flow:instance:read` |
// | 22 | GET | `/business/flow/task/done` | `business:flow:instance:read` |
// | 23 | GET | `/business/flow/task/cc` | `business:flow:instance:read` |
//
// **业务拒绝走引擎的 `rejected`**（不是 DbError）：所以这里统一
// `if let Some(error) = out.rejected { return Err(error) }`，把 `650xx` 码原样交给前端。

use crate::business::flow::engine;

// ---------- 行 → 出参 ----------

fn instance_res(row: &model::InstanceRow, my_task_id: Option<Uuid>) -> dto::InstanceRes {
    dto::InstanceRes {
        id: row.id.to_string(),
        instance_no: row.instance_no.clone(),
        flow_code: row.flow_code.clone(),
        flow_name: row.flow_name.clone(),
        version_no: row.version_no,
        biz_type: row.biz_type.clone(),
        biz_id: row.biz_id.map(|id| id.to_string()).unwrap_or_default(),
        biz_no: row.biz_no.clone().unwrap_or_default(),
        title: row.title.clone(),
        status: row.status.clone(),
        initiator_id: row.initiator_id.to_string(),
        initiator_name: row.initiator_name.clone().unwrap_or_default(),
        initiator_dept_id: row.initiator_dept_id.map(|id| id.to_string()).unwrap_or_default(),
        auto_passed_nodes: row.auto_passed_nodes,
        round_total: row.round_total,
        result_comment: row.result_comment.clone().unwrap_or_default(),
        form_data: row.form_data.clone(),
        started_at: row.started_at,
        ended_at: row.ended_at,
        created_at: row.created_at,
        my_task_id: my_task_id.map(|id| id.to_string()).unwrap_or_default(),
    }
}

fn node_progress_res(row: &model::NodeRow) -> dto::NodeProgressRes {
    dto::NodeProgressRes {
        node_key: row.node_key.clone(),
        node_name: row.node_name.clone(),
        node_type: row.node_type.clone(),
        round: row.round,
        status: row.status.clone(),
        approval_mode: row.approval_mode.clone().unwrap_or_default(),
        approve_ratio: row.approve_ratio.unwrap_or(0.0),
        approver_count: row.approver_count,
        approved_count: row.approved_count,
        rejected_count: row.rejected_count,
        is_auto_passed: row.is_auto_passed,
        taken_edge_key: row.taken_edge_key.clone().unwrap_or_default(),
        activated_at: row.activated_at,
        ended_at: row.ended_at,
        duration_ms: row.duration_ms,
    }
}

/// 待办 / 抄送条目的展示上下文（列表由 SQL JOIN 取齐，详情由实例行 + 节点表回填）。
struct TaskCtx<'a> {
    /// 单据号。
    instance_no: &'a str,
    /// 单据标题。
    title: &'a str,
    /// 审批业务类型（`biz_flow_instance.biz_type`；抄送路径没有实例行 → 空串）。
    biz_type: &'a str,
    /// 业务单据 id（空串 = 无）。
    biz_id: &'a str,
    /// 业务单据号（空串 = 无）。
    biz_no: &'a str,
    /// 流程名称。
    flow_name: &'a str,
    /// 发起人姓名。
    initiator_name: &'a str,
    /// 当前节点中文名。
    current_node_name: &'a str,
    /// 所属审批实例的状态（P11：前端防御要用，见 `dto::TaskRes::instance_status`）。
    instance_status: &'a str,
    /// 表单快照（待办卡片摘要用）。抄送出参（`CcRes`）没有这个字段 → 传 `None`。
    form_data: Option<&'a Value>,
}

fn task_res(row: &model::TaskRow, ctx: &TaskCtx<'_>) -> dto::TaskRes {
    dto::TaskRes {
        id: row.id.to_string(),
        instance_id: row.instance_id.to_string(),
        instance_no: ctx.instance_no.to_string(),
        title: ctx.title.to_string(),
        biz_type: ctx.biz_type.to_string(),
        biz_id: ctx.biz_id.to_string(),
        biz_no: ctx.biz_no.to_string(),
        flow_name: ctx.flow_name.to_string(),
        initiator_name: ctx.initiator_name.to_string(),
        current_node_name: ctx.current_node_name.to_string(),
        arrived_at: row.created_at,
        node_key: row.node_key.clone(),
        round: row.round,
        task_kind: row.task_kind.clone(),
        assignee_id: row.assignee_id.to_string(),
        assignee_name: row.assignee_name.clone().unwrap_or_default(),
        assignee_source: row.assignee_source.clone(),
        status: row.status.clone(),
        instance_status: ctx.instance_status.to_string(),
        // 没有实例行可取（抄送路径不用）→ `{}`（= 无表单），绝不 `null`：
        // 前端按「对象」判空，`null` 会走进另一条分支。
        form_data: ctx.form_data.cloned().unwrap_or_else(|| Value::Object(Map::new())),
        is_added_sign: row.is_added_sign,
        is_self_approval: row.is_self_approval,
        is_auto: row.is_auto,
        comment: row.comment.clone().unwrap_or_default(),
        transferred_from: row.transferred_from.map(|id| id.to_string()).unwrap_or_default(),
        escalated_from_role: row.escalated_from_role.clone().unwrap_or_default(),
        escalate_reason: row.escalate_reason.clone().unwrap_or_default(),
        escalated_to_role: row.escalated_to_role.clone().unwrap_or_default(),
        assigned_by_rule: row.assigned_by_rule.clone().unwrap_or_default(),
        created_at: row.created_at,
        completed_at: row.completed_at,
    }
}

fn cc_res(row: &model::CcRow, ctx: &TaskCtx<'_>) -> dto::CcRes {
    dto::CcRes {
        id: row.id.to_string(),
        instance_id: row.instance_id.to_string(),
        instance_no: ctx.instance_no.to_string(),
        title: ctx.title.to_string(),
        flow_name: ctx.flow_name.to_string(),
        initiator_name: ctx.initiator_name.to_string(),
        current_node_name: ctx.current_node_name.to_string(),
        arrived_at: row.created_at,
        node_key: row.node_key.clone(),
        round: row.round,
        is_read: row.is_read,
        read_at: row.read_at,
        created_at: row.created_at,
    }
}

fn action_res(out: engine::ActionOut) -> dto::ActionRes {
    dto::ActionRes {
        instance_id: out.instance_id.map(|id| id.to_string()).unwrap_or_default(),
        instance_status: out.instance_status,
        node_status: out.node_status.unwrap_or_default(),
        done: out.done,
        idempotent: out.idempotent,
        next_nodes: out.next_nodes,
        auto_passed_nodes: out.auto_passed_nodes,
        approver_count: out.approver_count,
        approved_count: out.approved_count,
        rejected_count: out.rejected_count,
    }
}

/// 事务跑完把锁内收集的副作用发出去（**锁外发送**，§4.5.1 第 ④ 条）。
async fn drain(out: &mut engine::ActionOut) -> Option<ApiError> {
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    // P-FLOW-2：通知节点的完整草稿（模板 / 变量 / 幂等键）。投递失败只 WARN，
    // 不影响已经提交的业务 —— 见 `engine::flush_drafts` 的头注释。
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    out.rejected.take()
}

/// 「发起时引擎业务拒绝」的哨兵错误文案（P-FLOW-1A）。
///
/// # 为什么需要这个哨兵
///
/// `engine::start` 把业务失败（如 `65018` 没审批人、`65039` 标题超长）放进
/// `out.rejected` 后**正常返回**，事务照样提交 —— 结果是「发起失败但留了一张 PENDING
/// 的空审批单」。对审批单只是难看，对 `TASK_NODE` 是「半截实例 + 半截任务」，
/// 而设计 §7.1 验收 7 明确要求**整笔回滚：`biz_task` 0 行、`biz_flow_instance` 0 行**。
///
/// `ApiError` 既没有 `Clone` 也不能塞进 `DbError`，所以用「哨兵错误 + 槽位」把它带出事务：
/// 事务内把原始 `ApiError` 挪进 `Arc<Mutex<Option<ApiError>>>`，再抛这个哨兵让事务回滚；
/// 事务外按哨兵认出这次失败，取出原始业务码原样返回前端（**码值与文案不变**）。
/// 哨兵文案的唯一来源是 [`engine::START_REJECTED_MARKER`]（发起路径与业务入口共用一套判据，
/// 免得两处字面量各自漂移）。见 [`engine::start_rejected_sentinel`]。
const START_REJECTED_MARKER: &str = engine::START_REJECTED_MARKER;

/// 是否「发起被引擎业务拒绝」的哨兵（见 [`START_REJECTED_MARKER`]）。
fn is_start_rejected(error: &DbError) -> bool {
    engine::is_start_rejected(error)
}

// ==================== 12. 发起 ====================

/// 发起一张审批单（`POST /business/flow/instance`）。
///
/// 流程解析三选一：`def_id` → `flow_code` → `biz_type` 的默认发布流程。
/// **只有 `PUBLISHED` 的流程能发起**（DRAFT 给 `400 + 65005`，不是 404）。
///
/// # 异常情况
///
/// - 400 + `65005`：字段非法 / 流程未发布 / `form_data` 不是对象；
/// - 404 + `65001`：流程不存在；
/// - 409 + `10003`：同一业务单据已有在跑的审批单（`uk_biz_flow_instance_biz_active`）。
#[nik::permission(
    path = "/business/flow/instance",
    method = "POST",
    permission = "business:flow:instance:create"
)]
#[nik::post("/business/flow/instance")]
pub async fn create_instance(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::InstanceCreateReq>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let title = dto::normalize_instance_title(&req.title).map_err(field_invalid)?;

    // ① 找流程定义
    let def = if let Some(raw_id) = req.def_id.as_deref().filter(|id| !id.trim().is_empty()) {
        let def_id = parse_uuid_field(raw_id, "流程 id")?;
        load_def(def_id).await?
    } else if let Some(code) = req.flow_code.as_deref().map(str::trim).filter(|code| !code.is_empty()) {
        match model::find_def_by_code(code)
            .await
            .map_err(|error| utils::db_error(error, "按编码查询流程定义"))?
        {
            Some(row) => row,
            None => return Err(model::not_found()),
        }
    } else if let Some(biz_type) = req.biz_type.as_deref().filter(|value| !value.trim().is_empty()) {
        let normalized = dto::normalize_biz_type(biz_type).map_err(field_invalid)?;
        match model::default_def_of_biz_type(&normalized)
            .await
            .map_err(|error| utils::db_error(error, "查询默认流程定义"))?
        {
            Some(row) => row,
            None => {
                return Err(ApiError::new(
                    404,
                    error_code::FLOW_NOT_FOUND,
                    format!("业务类型 {normalized} 没有默认的发布流程"),
                ));
            }
        }
    } else {
        return Err(field_invalid("必须指定流程：def_id / flow_code / biz_type 三选一"));
    };

    // ② 只有已发布的流程能发起（DRAFT 上的图还没过快照，引擎读不到图）
    let Some(version_id) = def.current_version_id else {
        return Err(field_invalid(format!(
            "流程 {} 尚未发布（status={}），不能发起审批",
            def.flow_code, def.status
        )));
    };
    if def.status != "PUBLISHED" {
        return Err(field_invalid(format!(
            "流程 {} 当前状态为 {}，只有 PUBLISHED 才能发起审批",
            def.flow_code, def.status
        )));
    }
    let version = model::find_version(version_id)
        .await
        .map_err(|error| utils::db_error(error, "查询流程版本"))?
        .ok_or_else(model::version_not_found)?;

    // ③ 业务单据：同一单据同时只能有一个在跑实例
    let biz_id = match req.biz_id.as_deref().filter(|value| !value.trim().is_empty()) {
        None => None,
        Some(raw) => Some(parse_uuid_field(raw, "业务单据 id")?),
    };
    if let Some(biz_id) = biz_id
        && model::biz_instance_running(&def.biz_type, biz_id)
            .await
            .map_err(|error| utils::db_error(error, "校验业务单据在跑实例"))?
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "该业务单据已有正在审批中的审批单，请先处理完",
        ));
    }

    // ④ 发起人快照
    let (initiator_name, initiator_dept_id) = model::user_profile(operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人信息"))?
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "发起人账号不存在"))?;

    let form_data = match req.form_data {
        None => JsonValue::Object(Map::new()),
        Some(value) if value.is_object() => value,
        Some(_) => return Err(field_invalid("form_data 必须是 JSON 对象")),
    };
    let biz_no = req
        .biz_no
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let biz_type = req
        .biz_type
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(dto::normalize_biz_type)
        .transpose()
        .map_err(field_invalid)?
        .unwrap_or_else(|| def.biz_type.clone());

    let instance_id = Uuid::now_v7();
    // 业务拒绝的「带出槽」（见 START_REJECTED_MARKER 的说明）：事务内挪进 ApiError，
    // 事务外取出来原样返回给前端。
    let rejected_slot: std::sync::Arc<std::sync::Mutex<Option<ApiError>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let rejected_slot_tx = std::sync::Arc::clone(&rejected_slot);
    let start = engine::StartReq {
        instance_id,
        def_id: def.id,
        version_id,
        version_no: version.version_no,
        flow_code: def.flow_code.clone(),
        flow_name: def.flow_name.clone(),
        biz_type,
        biz_id,
        biz_no,
        title: title.clone(),
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data,
    };

    let result = db()
        .transaction(move |tx| async move {
            let mut out = engine::start(&tx, &start).await?;
            if let Some(error) = out.rejected.take() {
                // 业务失败 → 整笔回滚（不留半截实例 / 半截任务）。
                // 原始业务码经槽位带出事务（`ApiError` 不能 Clone 也不能进 DbError）。
                if let Ok(mut slot) = rejected_slot_tx.lock() {
                    *slot = Some(error);
                }
                return Err(DbError::InvalidValue(START_REJECTED_MARKER.to_string()));
            }
            Ok(out)
        })
        .await;
    let mut out = match result {
        Ok(out) => out,
        Err(error) if is_start_rejected(&error) => {
            let rejected = rejected_slot.lock().ok().and_then(|mut slot| slot.take());
            return Err(rejected.unwrap_or_else(|| utils::db_error(error, "发起审批")));
        }
        // 判据只有一份实现（`flow::model`）：变体优先 + 文案兜底。
        Err(error) if model::is_biz_active_duplicate(&error) => {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "该业务单据已有正在审批中的审批单（并发提交被数据库挡下）",
            ));
        }
        Err(error) => return Err(utils::db_error(error, "发起审批")),
    };
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // ★ LNK-12：发起时**整条流程自动走完**（全节点 `SKIP` / 空指派自动通过）也会落终态
    //   `APPROVED` —— 那一瞬间没有任何 approve handler 可走，只能在这里补一次；
    //   否则就是「配了 FLOW_INSTANCE/APPROVED 的规则有时响、有时不响」。
    //   正常发起（停在 PENDING）时 `instance_status = PENDING`，hook 直接返回 0。
    instance_linkage::fire_after_commit(out.instance_id, &out.instance_status, Some(operator)).await;
    tracing::info!(
        %instance_id,
        flow_code = %def.flow_code,
        %operator,
        status = %out.instance_status,
        auto_passed = out.auto_passed_nodes,
        "审批单已发起"
    );
    Ok(Res::ok(action_res(out)))
}

// ==================== 13. 分页 ====================

/// 实例分页（**可见性过滤**：数据范围 ∪ 我发起的 ∪ 我待办/已办 ∪ 抄送我）。
///
/// # 异常情况
///
/// - 400 + `65005`：状态 / 业务类型取值非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/instance/page",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/page")]
pub async fn page_instances(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::InstancePageQuery>,
) -> ResResult<dto::InstancePageRes> {
    let me = me_of(&claims)?;
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let status = match query.status.as_deref() {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(dto::normalize_instance_status(raw).ok_or_else(|| {
            ApiError::new(
                400,
                error_code::FLOW_FIELD_INVALID,
                format!("状态 {raw} 非法（只允许 PENDING / APPROVED / REJECTED / CANCELLED / WITHDRAWN）"),
            )
        })?),
    };
    let biz_type = match query.biz_type.as_deref() {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(dto::normalize_biz_type(raw).map_err(field_invalid)?),
    };
    let filter = model::InstanceFilter {
        keyword: query.normalized_keyword(),
        status,
        flow_code: query
            .flow_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        biz_type,
    };
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_instances(&principal, &filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询审批单"))?;
    let mut list = Vec::with_capacity(result.list.len());
    for row in &result.list {
        let my_task = model::my_task_of(row.id, me)
            .await
            .map_err(|error| utils::db_error(error, "查询我的待办"))?;
        list.push(instance_res(row, my_task.map(|task| task.id)));
    }
    Ok(Res::ok(dto::InstancePageRes {
        list,
        total: result.total,
        page: result.page,
        page_size: result.size,
    }))
}

// ==================== 14. 详情 ====================

/// 审批单详情（实例 + 节点进度 + 时间线 + 抄送 + 我的待办）。
///
/// 可见性：发起人 / 审批人（含已审）/ 抄送人 / 流程管理员（架构 §9.2）。
///
/// # 异常情况
///
/// - 400 + `65005`：id 不是 UUID；
/// - 404 + `65002`：审批单不存在；
/// - 403 + `65004`：与本单无关。
#[nik::permission(
    path = "/business/flow/instance/{id}",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/{id}")]
pub async fn instance_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::InstanceDetailRes> {
    let me = me_of(&claims)?;
    let instance_id = parse_uuid_field(&id, "审批单 id")?;
    let row = model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .ok_or_else(|| ApiError::new(404, error_code::FLOW_INSTANCE_NOT_FOUND, "审批单不存在"))?;

    let mine = model::my_instance_ids(me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的相关审批单"))?;
    let allowed = row.initiator_id == me || mine.contains(&row.id) || claims.has_permission("business:flow:admin");
    if !allowed {
        return Err(ApiError::new(
            403,
            error_code::FLOW_FORBIDDEN,
            "你与这张审批单无关，不能查看",
        ));
    }

    let nodes = model::nodes_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询节点进度"))?;
    let timeline = model::timeline(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批时间线"))?;
    let cc = model::cc_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询抄送"))?;
    let my_task = model::my_task_of(row.id, me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的待办"))?;

    // 节点中文名：详情的时间线 / 抄送不带 JOIN，用本单节点行做一次内存映射
    // （同一 node_key 的 node_name 是快照固定值，跨轮次不会变）。
    let node_names = nodes
        .iter()
        .map(|node| (node.node_key.as_str(), node.node_name.as_str()))
        .collect::<std::collections::HashMap<&str, &str>>();
    let initiator_name = row.initiator_name.clone().unwrap_or_default();
    let biz_id = row.biz_id.map(|id| id.to_string()).unwrap_or_default();
    let biz_no = row.biz_no.clone().unwrap_or_default();
    let ctx_of = |node_key: &str| TaskCtx {
        instance_no: row.instance_no.as_str(),
        title: row.title.as_str(),
        biz_type: row.biz_type.as_str(),
        biz_id: biz_id.as_str(),
        biz_no: biz_no.as_str(),
        flow_name: row.flow_name.as_str(),
        initiator_name: initiator_name.as_str(),
        current_node_name: node_names.get(node_key).copied().unwrap_or(""),
        instance_status: row.status.as_str(),
        // 时间线挂在同一个实例下：详情里已经取了实例行，直接复用，零额外查询。
        form_data: Some(&row.form_data),
    };

    Ok(Res::ok(dto::InstanceDetailRes {
        instance: instance_res(&row, my_task.as_ref().map(|task| task.id)),
        nodes: nodes.iter().map(node_progress_res).collect(),
        timeline: timeline
            .iter()
            .map(|task| task_res(task, &ctx_of(&task.node_key)))
            .collect(),
        cc: cc.iter().map(|entry| cc_res(entry, &ctx_of(&entry.node_key))).collect(),
        my_task: my_task.as_ref().map(|task| task_res(task, &ctx_of(&task.node_key))),
        auto_passed_nodes: row.auto_passed_nodes,
    }))
}

// ==================== 14b. 按业务单据反查审批单（VTRACE-5 全链路视图） ====================

/// 按业务单据反查它的审批单（`GET /business/flow/instance/by-biz`）。
///
/// 全链路视图（凭证 → 来源单据 → 审批 → 分录）需要的只有一件事：
/// **这张业务单据走没走审批、谁审的、什么时间、什么结论**。
/// 前端手上只有 `biz_voucher.source_id / source_no`，分页接口又搜不到业务单号，
/// 于是补这个直查口子。
///
/// # 可见性（与 [`instance_detail`] 同一口径，但**不 403**）
///
/// 发起人 / 审批人（含已审）/ 抄送人 / 流程管理员 → `visible = true` + 时间线；
/// 其余人 → `visible = false` + 空时间线。理由：这是**我这边凭证的追溯视图**，
/// 无权看审批明细不该让整屏 403 失败，但也不能假装「这张单没有审批」——
/// 两种语义完全不同，所以用一个显式布尔值说清楚。审批单号 / 状态 / 发起人
/// 属于「审批存在过」这一事实，仍然下发。
///
/// # 异常情况
///
/// - 400 + `65005`：`biz_id` / `biz_no` 都没给、`biz_id` 不是 uuid、`biz_type` 非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/instance/by-biz",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/by-biz")]
pub async fn instance_by_biz(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::InstanceByBizQuery>,
) -> ResResult<dto::InstanceTraceListRes> {
    let me = me_of(&claims)?;
    let (biz_id, biz_no) =
        dto::normalize_biz_lookup(query.biz_id.as_deref(), query.biz_no.as_deref()).map_err(field_invalid)?;
    let biz_type = match query.biz_type.as_deref() {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(dto::normalize_biz_type(raw).map_err(field_invalid)?),
    };
    let rows = model::instances_of_biz(biz_type.as_deref(), biz_id, biz_no.as_deref())
        .await
        .map_err(|error| utils::db_error(error, "按业务单据反查审批单"))?;
    let mine = model::my_instance_ids(me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的相关审批单"))?;
    let is_admin = claims.has_permission("business:flow:admin");
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let visible = row.initiator_id == me || mine.contains(&row.id) || is_admin;
        // 节点中文名只在时间线上用；无权的人不发这次查询（少一次无意义的往返）。
        let tasks = if visible {
            let nodes = model::nodes_of(row.id)
                .await
                .map_err(|error| utils::db_error(error, "查询节点进度"))?;
            let names = nodes
                .iter()
                .map(|node| (node.node_key.as_str(), node.node_name.as_str()))
                .collect::<std::collections::HashMap<&str, &str>>();
            let timeline = model::timeline(row.id)
                .await
                .map_err(|error| utils::db_error(error, "查询审批时间线"))?;
            timeline
                .iter()
                .map(|task| dto::InstanceTraceTaskRes {
                    node_key: task.node_key.clone(),
                    node_name: names
                        .get(task.node_key.as_str())
                        .copied()
                        .unwrap_or(task.node_key.as_str())
                        .to_string(),
                    assignee_name: task.assignee_name.clone().unwrap_or_default(),
                    status: task.status.clone(),
                    // 未完成的待办 `completed_at` 是空的：显示到岗时间才能看出「在他手上多久」，
                    // 与 `shared/approval-progress.ts::mapApprovalTasks` 同一口径。
                    at: Some(task.completed_at.unwrap_or(task.created_at)),
                    comment: task.comment.clone().unwrap_or_default(),
                    is_auto: task.is_auto,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        list.push(dto::InstanceTraceRes {
            id: row.id.to_string(),
            instance_no: row.instance_no.clone(),
            flow_name: row.flow_name.clone(),
            status: row.status.clone(),
            started_at: row.started_at,
            ended_at: row.ended_at,
            initiator_name: row.initiator_name.clone().unwrap_or_default(),
            biz_type: row.biz_type.clone(),
            biz_no: row.biz_no.clone().unwrap_or_default(),
            visible,
            tasks,
        });
    }
    Ok(Res::ok(dto::InstanceTraceListRes { list }))
}

// ==================== 15. 撤回 ====================

/// 撤回（只允许发起人本人 + 零人已审，架构 D9）。
///
/// # 异常情况
///
/// - 403 + `65004`：不是发起人；
/// - 409 + `65023`：已有人审批过；
/// - 409 + `65024`：审批单已结束。
#[nik::permission(
    path = "/business/flow/instance/{id}/withdraw",
    method = "POST",
    permission = "business:flow:instance:withdraw"
)]
#[nik::post("/business/flow/instance/{id}/withdraw")]
pub async fn withdraw_instance(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<dto::WithdrawReq>>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let instance_id = parse_uuid_field(&id, "审批单 id")?;
    let reason = match body {
        None => None,
        Some(Json(req)) => dto::normalize_comment(req.reason.as_deref()).map_err(field_invalid)?,
    };
    let _ = reason;
    let mut out = db()
        .transaction(|tx| async move { engine::withdraw(&tx, instance_id, operator).await })
        .await
        .map_err(|error| utils::db_error(error, "撤回审批单"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // ★ LNK-12：审批实例落终态（发起人撤回 = WITHDRAWN）→ `FLOW_INSTANCE` 触发点。
    //   位置在业务桥之前：撤回同样要通知发起人 / 相关人；业务桥随后把单据打回草稿。
    instance_linkage::fire_after_commit(out.instance_id, &out.instance_status, Some(operator)).await;
    // P16：从审批中心撤回的也可能是一张「收货单作废」单 → 单据必须回到已过账。
    // 不做这一步就是 P11 的老坑：审批单 WITHDRAWN 了，单据永远停在「作废审批中」。
    crate::business::erp::stockin_bridge::on_cancel_settled(Some(instance_id), "WITHDRAWN").await;
    // P-FLOW-3A：也可能是「收货过账」单 → 打回草稿（账一分没动）
    crate::business::erp::stockin_bridge::on_post_settled(Some(instance_id), "WITHDRAWN").await;
    // P-FLOW-3A：也可能是「采购订单审批」单 → 打回草稿
    crate::business::erp::purchase_bridge::on_po_settled(Some(instance_id), "WITHDRAWN").await;
    // P2-02：也可能是「采购申请审批」单 → 打回草稿（可以改完重提）
    crate::business::erp::requisition_bridge::on_pr_settled(Some(instance_id), "WITHDRAWN").await;
    // P2-03：也可能是「采购退货审批」单 → 打回草稿（账一分没动，可以改完重提）
    crate::business::erp::purchase_return_bridge::on_settled(Some(instance_id), "WITHDRAWN").await;
    // SO-06：也可能是「销售订单审批」单 → 打回草稿（清实例，改完可重新提交）
    crate::business::erp::sales_approval_bridge::on_so_settled(Some(instance_id), "WITHDRAWN").await;
    // FIN-1：也可能是「预算审批」单 → 打回草稿（额度一分没生效）
    crate::business::erp::budget_bridge::on_budget_settled(Some(instance_id), "WITHDRAWN").await;
    // FIN-2：也可能是「资产处置审批」单 → 撤回即资产原地不动（一张凭证都不生成）
    crate::business::erp::asset_bridge::on_disposal_settled(Some(instance_id), "WITHDRAWN").await;
    // FIN-3：也可能是「费用报销」单 → 撤回即回草稿并释放预算占用（不入账）
    crate::business::erp::expense_bridge::on_expense_settled(Some(instance_id), "WITHDRAWN").await;
    // HR-27：也可能是「请假审批」单 → 撤回即回草稿（等 HR 改完重提）
    crate::business::hr::leave_bridge::on_leave_settled(Some(instance_id), "WITHDRAWN").await;
    // HR-39：也可能是「薪资单审批」单 → 撤回即退回 CALCULATED 重算（账一分没动）
    crate::business::hr::salary_bridge::on_salary_settled(Some(instance_id), "WITHDRAWN").await;
    // B3-3：也可能是「成本结算」单 → 撤回即批次回草稿 + 清空审批痕迹（账一分没动）
    crate::business::erp::cost_period_bridge::on_cost_settle_decided(Some(instance_id), "WITHDRAWN").await;
    // ARCH-3 R-P1-1：也可能是「合同审批」单 → 撤回即回草稿（实例清空，可改后重提）
    crate::business::erp::contract_bridge::on_contract_settled(Some(instance_id), "WITHDRAWN").await;
    // ARCH-3 DC-14：也可能是「盘点过账审批」单 → 撤回即回草稿（账一分没动，可改实盘数后重提）
    crate::business::erp::stocktake_bridge::on_stock_take_settled(Some(instance_id), "WITHDRAWN").await;
    // P17：也可能是「应付付款」单 → 付款记录标 REJECTED（账一分没动，可重新提交）
    crate::business::erp::payable_bridge::on_payment_settled(Some(instance_id), "WITHDRAWN").await;
    // SO-14 遗留 D10：也可能是「收款退款」单 → 退款单仍是已确认（账一分没动，可改完再提）
    crate::business::erp::receipt_refund_bridge::on_refund_settled(Some(instance_id), "WITHDRAWN").await;
    // P-FLOW-3B：也可能是「应付作废」单 → 单据回 OPEN（欠款一分没动）
    crate::business::erp::payable_bridge::on_cancel_settled(Some(instance_id), "WITHDRAWN").await;
    // P-FLOW-3B：也可能是「凭证红冲」单 → 只清 pending（凭证一直是 POSTED，账没动）
    crate::business::erp::voucher_bridge::on_reverse_settled(Some(instance_id), "WITHDRAWN").await;
    // D3：也可能是「状态机配置变更」单 → 配置一分不动（撤回 = 不改流程规则）
    crate::business::flow::edge_admin::on_settled(Some(instance_id), "WITHDRAWN").await;
    Ok(Res::ok(action_res(out)))
}

// ==================== 15b. 终止（流程管理员人工终止） ====================

/// 人工终止在跑的审批单（`business:flow:admin`）。
///
/// # 为什么必须有它（E2E-01 演练实测的痛点）
///
/// 会签节点缺人 → 提交后永远批不完。此时 `withdraw` 会被 `409 + 65023` 拦下
/// （文案原文就是「…请联系流程管理员终止」），而在此之前**没有任何 HTTP 入口**能终止 ——
/// 只能改库。本接口就是那句文案的兑现：只要能拿到 `business:flow:admin`，就能把
/// 「卡住的实例」收掉，并且**关联业务单据必须跟着退回**（ARCH-8 约定 11 的延伸：
/// 不只是「失败要兜底」，还要「卡住了要有出路」）。
///
/// # 权限码为什么复用 `business:flow:admin`
///
/// 它在 V0124 里的原始注释就写着「流程管理员（退回任意节点 / **终止**）」，
/// 是唯一已种进 `sys_menu`（…0804）且默认只授 SUPER_ADMIN 的管理码；
/// `reject` 的「驳回并终止」分支也用它做行级判定。新开一个
/// `business:flow:instance:terminate` 需要额外迁移 + 角色授权决策，
/// 效果与现在完全一样（没有角色被授予时同样只有超管能用），只是多一个码要维护。
///
/// # 实例落 `CANCELLED`
///
/// DDL 的 CHECK 只有 `PENDING/APPROVED/REJECTED/CANCELLED/WITHDRAWN`，`CANCELLED`
/// 在全仓的语义就是「行政终止」，15 个业务桥的回调判据都包含它。详见
/// [`engine::terminate_by_admin`] 的头注释（为什么不新增 `TERMINATED` 字面量）。
///
/// # 异常情况
///
/// - 400 + `65005`：原因空 / 超长、id 格式非法；
/// - 404 + `65002`：审批单不存在；
/// - 400 + `65024`：审批单已结束（终态不可再终止，卡面验收⑥）；
/// - 403：没有 `business:flow:admin`。
#[nik::permission(
    path = "/business/flow/instance/{id}/terminate",
    method = "POST",
    permission = "business:flow:admin"
)]
#[nik::post("/business/flow/instance/{id}/terminate")]
pub async fn terminate_instance(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::TerminateReq>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let instance_id = parse_uuid_field(&id, "审批单 id")?;
    let reason = dto::normalize_terminate_reason(&req.reason).map_err(field_invalid)?;

    // ① 先做一次「还在不在跑」的显式判定（事务外，读一致性足够）：
    //    终态 → 400 + 65024。引擎里还有一次 `is_pending` + revision CAS 兜并发，
    //    这里早拦是为了给前端一个明确的 400（而不是等引擎回 409）。
    let current = model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "终止审批单"))?
        .ok_or_else(|| ApiError::new(404, error_code::FLOW_INSTANCE_NOT_FOUND, "流程实例不存在"))?;
    if !current.is_pending() {
        return Err(ApiError::new(
            400,
            error_code::FLOW_INSTANCE_NOT_PENDING,
            format!("流程实例已结束（{}），不能终止", current.status),
        ));
    }

    let reason_in_tx = reason.clone();
    let mut out = db()
        .transaction(|tx| async move { engine::terminate_by_admin(&tx, instance_id, operator, &reason_in_tx).await })
        .await
        .map_err(|error| utils::db_error(error, "终止审批单"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }

    // ★ 事务已提交（实例 = CANCELLED）。下面每一步都是「提交后补偿」：
    //   失败只记日志、不回滚审批单 —— 与 withdraw / approve 同一口径。
    //   全量调用（不按 biz_type 分流）：每一张桥自己判「这单是不是我的」，
    //   判据里 `REJECTED | WITHDRAWN | CANCELLED` 一律「单据退回」。
    //   这就是 ARCH-8 约定 11 的硬要求：**流程终止，单据不许停在 PENDING_\***。
    crate::business::erp::stockin_bridge::on_cancel_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::stockin_bridge::on_post_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::purchase_bridge::on_po_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::requisition_bridge::on_pr_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::purchase_return_bridge::on_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::sales_approval_bridge::on_so_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::budget_bridge::on_budget_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::asset_bridge::on_disposal_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::expense_bridge::on_expense_settled(Some(instance_id), "CANCELLED").await;
    // HR-27：请假审批被行政终止 → 单据回草稿（不许停在「审批中」却没人能批）
    crate::business::hr::leave_bridge::on_leave_settled(Some(instance_id), "CANCELLED").await;
    // HR-39：薪资单审批被行政终止 → 退回 CALCULATED 重算（不许停在「审批中」却没人能批）
    crate::business::hr::salary_bridge::on_salary_settled(Some(instance_id), "CANCELLED").await;
    // B3-3：成本结算审批被行政终止 → 批次回草稿（不能停在「审批中」却没人能批）
    crate::business::erp::cost_period_bridge::on_cost_settle_decided(Some(instance_id), "CANCELLED").await;
    crate::business::erp::contract_bridge::on_contract_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::stocktake_bridge::on_stock_take_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::payable_bridge::on_payment_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::receipt_refund_bridge::on_refund_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::payable_bridge::on_cancel_settled(Some(instance_id), "CANCELLED").await;
    crate::business::erp::voucher_bridge::on_reverse_settled(Some(instance_id), "CANCELLED").await;
    crate::business::flow::edge_admin::on_settled(Some(instance_id), "CANCELLED").await;
    // 子流程实例被终止 → 父流程必须知道（否则父节点永远等一个不可能再有结论的子单）
    crate::business::flow::subprocess::on_instance_settled(Some(instance_id), "CANCELLED").await;
    // 任务验收类审批单被终止 → 任务侧不做状态推进（settle_target 不认 CANCELLED），
    // 但 `biz_flow_task` / 节点已由引擎的 `cancel_flow_tasks` 收尾，这里只对齐留痕。
    task_bridge::on_instance_settled(Some(instance_id), "CANCELLED").await;

    tracing::info!(
        %instance_id,
        %operator,
        reason = %reason,
        "审批单已由流程管理员人工终止（CANCELLED）"
    );
    Ok(Res::ok(action_res(out)))
}

// ==================== 16. 同意 ====================

/// 同意（含会签：`ALL` / `RATIO` 达到阈值即通过，`ANY` 一票即过）。
///
/// # 异常情况
///
/// - 403 + `65028`：这条待办不是你的；
/// - 404 + `65003`：待办不存在；
/// - 409 + `65024`：审批单已结束。
#[nik::permission(
    path = "/business/flow/task/{id}/approve",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/{id}/approve")]
pub async fn approve_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<dto::ApproveReq>>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let task_id = parse_uuid_field(&id, "待办 id")?;
    let comment = match body {
        None => None,
        Some(Json(req)) => dto::normalize_comment(req.comment.as_deref()).map_err(field_invalid)?,
    };
    // ★ P17 内控闸（在事务之前）：付款审批的**制单人 / 发起人**不能批自己那一票 → 403。
    // 判据只认 `biz_payment.create_by`（与实例发起人），与本业务无关的待办零影响。
    crate::business::erp::payable_bridge::guard_self_approval(task_id, operator).await?;
    // ★ P-FLOW-3A 内控闸：采购订单审批 / 收货过账审批同款（制单人 ≠ 审批人）。
    crate::business::erp::purchase_bridge::guard_self_approval(task_id, operator).await?;
    crate::business::erp::stockin_bridge::guard_self_approval(task_id, operator).await?;
    // ★ P-FLOW-3B 内控闸：凭证红冲同款（制单人 ≠ 审批人）。
    //   应付作废走 payable_bridge::guard_self_approval（内部按 biz_type 分流）。
    crate::business::erp::voucher_bridge::guard_self_approval(task_id, operator).await?;
    // ★ ARCH-3 R-P1-1 内控闸：合同审批同款（制单人 / 发起人 ≠ 审批人）。
    crate::business::erp::contract_bridge::guard_self_approval(task_id, operator).await?;
    // ARCH-3 DC-14：盘点过账审批 —— 盘的人不能批自己盘的账（403 + 65019）
    crate::business::erp::stocktake_bridge::guard_self_approval(task_id, operator).await?;
    // FIN-1：预算审批 —— 编预算的人不能批自己编的预算（403 + 65019）
    crate::business::erp::budget_bridge::guard_self_approval(task_id, operator).await?;
    // FIN-2：资产处置 —— 发起人 / 制单人不能批自己的处置（403 + 65019）
    crate::business::erp::asset_bridge::guard_self_approval(task_id, operator).await?;
    // FIN-3：费用报销 —— 报销人 / 发起人不能批自己的报销（403 + 65019）
    crate::business::erp::expense_bridge::guard_self_approval(task_id, operator).await?;
    // HR-27：请假审批 —— 发起人 / 请假员工本人不能批自己的请假（403 + 65019）
    crate::business::hr::leave_bridge::guard_self_approval(task_id, operator).await?;
    // HR-39：薪资单 —— 发起人不能批自己提交的工资单（设计 §4.3 第 2 条）
    crate::business::hr::salary_bridge::guard_self_approval(task_id, operator).await?;
    // SO-06：销售订单 —— 制单人 / 发起人不能批自己的销售订单（403 + 65019）
    crate::business::erp::sales_approval_bridge::guard_self_approval(task_id, operator).await?;
    // SO-14 遗留 D10：收款退款 —— 制单人 / 发起人不能批自己的退款（403 + 65019）
    crate::business::erp::receipt_refund_bridge::guard_self_approval(task_id, operator).await?;
    // B3-3：成本结算 —— 制单人 / 发起人不能批自己的结算（财务经理本人制单的场景）
    crate::business::erp::cost_period_bridge::guard_self_approval(task_id, operator).await?;
    // P2-02：采购申请 —— 制单人 / 发起人不能批自己的申请（403 + 65019）
    crate::business::erp::requisition_bridge::guard_self_approval(task_id, operator).await?;
    // P2-03：采购退货 —— 制单人 / 发起人不能批自己的退货（403 + 65019）
    crate::business::erp::purchase_return_bridge::guard_self_approval(task_id, operator).await?;
    let mut out = db()
        .transaction(|tx| async move { engine::approve(&tx, task_id, operator, comment.as_deref()).await })
        .await
        .map_err(|error| utils::db_error(error, "审批同意"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // ★ LNK-12：审批实例落终态（APPROVED）→ `FLOW_INSTANCE` 触发点。
    //   位置刻意在**业务桥之前**：实例是「原因」、单据状态是「结果」；业务桥的错误被吞掉，
    //   排在它们后面就会「桥挂了 → 审批通过的通知永远丢」。设计见 flow/instance_linkage.rs 文件头。
    instance_linkage::fire_after_commit(out.instance_id, &out.instance_status, Some(operator)).await;
    // D3b：审批单可能已落终态（APPROVED）→ 以 FlowCallback 身份推进任务
    // （**事务已提交**：回调失败只记日志，不回滚审批单）
    task_bridge::on_instance_settled(out.instance_id, &out.instance_status).await;
    // ★ P3-08：这单也可能是**子流程实例** —— 结算了就回调父流程的 SUBPROCESS 节点
    //   （幂等：父实例已终结 / 父节点已是终态 → 静默跳过；失败只 warn）
    crate::business::flow::subprocess::on_instance_settled(out.instance_id, &out.instance_status).await;
    // W3b：同一条审批单也可能是「库存调拨单」发起的 —— 与任务回调并列、互不影响
    task_bridge::on_transfer_settled(out.instance_id, &out.instance_status).await;
    // P16：也可能是「收货单作废」发起的 —— 通过则执行六本账回滚，驳回则打回已过账
    crate::business::erp::stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3A：也可能是「收货过账」发起的 —— 通过才过账（库存/凭证/应付一起动）
    crate::business::erp::stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3A：也可能是「采购订单审批」发起的 —— 通过则 APPROVED（可收货），驳回则打回草稿
    crate::business::erp::purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
    // P2-02：也可能是「采购申请审批」发起的 —— 通过则 APPROVED（可转采购订单），驳回则打回草稿
    crate::business::erp::requisition_bridge::on_pr_settled(out.instance_id, &out.instance_status).await;
    // P2-03：也可能是「采购退货审批」发起的 —— 通过则 APPROVED（可确认退货），驳回则打回草稿
    crate::business::erp::purchase_return_bridge::on_settled(out.instance_id, &out.instance_status).await;
    // SO-06：也可能是「销售订单审批」发起的 —— 通过则 APPROVED（可出库），驳回则打回草稿
    crate::business::erp::sales_approval_bridge::on_so_settled(out.instance_id, &out.instance_status).await;
    // FIN-1（ARCH-6 §3.3）：也可能是「预算审批」发起的 —— 通过则 ACTIVE（同槽旧版 SUPERSEDED）
    crate::business::erp::budget_bridge::on_budget_settled(out.instance_id, &out.instance_status).await;
    // FIN-2（ARCH-6 §5.6.3）：也可能是「资产处置」发起的 —— 通过则生成处置凭证 + 卡片 DISPOSED
    crate::business::erp::asset_bridge::on_disposal_settled(out.instance_id, &out.instance_status).await;
    // FIN-3（ARCH-6 §4.4.4）：也可能是「费用报销」发起的 —— 通过则入账（计提凭证 + 预算执行）
    crate::business::erp::expense_bridge::on_expense_settled(out.instance_id, &out.instance_status).await;
    // HR-27（ARCH-9 §4.3）：也可能是「请假审批」发起的 —— 通过则 SUBMITTED→APPROVED（不改考勤）
    crate::business::hr::leave_bridge::on_leave_settled(out.instance_id, &out.instance_status).await;
    // HR-39（ARCH-9 §4.3）：也可能是「薪资单审批」发起的 —— 通过则 SUBMITTED→APPROVED
    // （**不出凭证**，凭证在人工过账时生成）；驳回 / 终止则退回 CALCULATED 重算
    crate::business::hr::salary_bridge::on_salary_settled(out.instance_id, &out.instance_status).await;
    // B3-3：也可能是「成本结算」发起的 —— 通过则批次 APPROVED（数据冻结，等 B3-4 结算）
    crate::business::erp::cost_period_bridge::on_cost_settle_decided(out.instance_id, &out.instance_status).await;
    // ARCH-3 R-P1-1：也可能是「合同审批」发起的 —— 通过则 SIGNED（可被采购单引用），驳回则回草稿
    crate::business::erp::contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;
    // ARCH-3 DC-14：也可能是「盘点过账审批」发起的 —— 通过只是「允许过账」（状态仍待过账），
    // 驳回/终止则回草稿（三本账一分没动）
    crate::business::erp::stocktake_bridge::on_stock_take_settled(out.instance_id, &out.instance_status).await;
    // P17：也可能是「应付付款」发起的 —— 通过才记账（应付回写 + 借 2202 / 贷 1002 凭证）
    crate::business::erp::payable_bridge::on_payment_settled(out.instance_id, &out.instance_status).await;
    // SO-14 遗留 D10：也可能是「收款退款」发起的 —— 通过则由回调**直接过账**
    // （借 1122 / 贷 1002 凭证），不需要人再点一次过账；驳回 / 终止则退款单不动
    crate::business::erp::receipt_refund_bridge::on_refund_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3B：也可能是「应付作废」发起的 —— 通过才真正作废（进项凭证红冲 + 欠款消失）
    crate::business::erp::payable_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3B：也可能是「凭证红冲」发起的 —— 通过才生成红字凭证（原凭证保持 POSTED）
    crate::business::erp::voucher_bridge::on_reverse_settled(out.instance_id, &out.instance_status).await;
    // D3：也可能是「状态机配置变更」发起的 —— 通过才真正写 biz_flow_state_edge（驳回则一分不动）
    crate::business::flow::edge_admin::on_settled(out.instance_id, &out.instance_status).await;
    Ok(Res::ok(action_res(out)))
}

// ==================== 17. 驳回 ====================

/// 驳回（必填意见 ≥ 5 字 + 退回目标，架构 §4.6）。
///
/// `return_to`：`PREV`（默认，退上一节点）/ `INITIATOR`（退发起人，生成 `RESUBMIT` 待办）/
/// `NONE`（驳回并终止，需 `business:flow:admin`）/ 节点 key（退回指定节点，需 `business:flow:admin`）。
///
/// # 异常情况
///
/// - 400 + `65005`：意见少于 5 字；
/// - 403 + `65004`：选了「退任意节点 / 终止」但不是流程管理员；
/// - 403 + `65028`：这条待办不是你的；
/// - 400 + `65025`：退回目标节点不在图上。
#[nik::permission(
    path = "/business/flow/task/{id}/reject",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/{id}/reject")]
pub async fn reject_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::RejectReq>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let task_id = parse_uuid_field(&id, "待办 id")?;
    let comment = dto::normalize_reject_comment(&req.comment).map_err(field_invalid)?;
    let return_to = engine::ReturnTo::parse(req.return_to.as_deref().unwrap_or(""));
    if return_to.needs_admin() && !claims.has_permission("business:flow:admin") {
        return Err(ApiError::new(
            403,
            error_code::FLOW_FORBIDDEN,
            "退回任意节点 / 驳回并终止需要流程管理员权限（business:flow:admin）",
        ));
    }
    let mut out = db()
        .transaction(|tx| async move { engine::reject(&tx, task_id, operator, &comment, return_to).await })
        .await
        .map_err(|error| utils::db_error(error, "审批驳回"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // ★ LNK-12：审批实例落终态（驳回并终止 = REJECTED）→ `FLOW_INSTANCE` 触发点。
    //   位置在业务桥之前（与 approve_task 同口径）；退回上一节点仍是 PENDING → 不触发。
    instance_linkage::fire_after_commit(out.instance_id, &out.instance_status, Some(operator)).await;
    // D3b：驳回并终止（REJECTED）→ 以 FlowCallback 身份把任务打回 DOING
    // （「退回上一节点」仍是 PENDING，回调不推进；落到 REJECTED 才动任务）
    task_bridge::on_instance_settled(out.instance_id, &out.instance_status).await;
    // ★ P3-08：子流程实例被驳回并终止 → 按 fail_action 回调父节点（ABORT 会让父实例终止）
    crate::business::flow::subprocess::on_instance_settled(out.instance_id, &out.instance_status).await;
    // W3b：同一条审批单也可能是「库存调拨单」发起的 —— 与任务回调并列、互不影响
    task_bridge::on_transfer_settled(out.instance_id, &out.instance_status).await;
    // P16：收货单作废被驳回 → 单据回到已过账（账一动没动）
    crate::business::erp::stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3A：收货过账被驳回 → 单据打回草稿（六本账一动没动）
    crate::business::erp::stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3A：采购订单审批被驳回 → 打回草稿 + 审批意见写 remark
    crate::business::erp::purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
    // P2-02：采购申请审批被驳回 → 打回草稿 + 审批意见写 remark
    crate::business::erp::requisition_bridge::on_pr_settled(out.instance_id, &out.instance_status).await;
    // P2-03：采购退货审批被驳回 → 打回草稿 + 审批意见写 remark
    crate::business::erp::purchase_return_bridge::on_settled(out.instance_id, &out.instance_status).await;
    // SO-06：销售订单审批被驳回 → 打回草稿 + 审批意见写 remark
    crate::business::erp::sales_approval_bridge::on_so_settled(out.instance_id, &out.instance_status).await;
    // FIN-1：预算审批被驳回 / 撤回 → 打回草稿（额度一分没生效）
    crate::business::erp::budget_bridge::on_budget_settled(out.instance_id, &out.instance_status).await;
    // FIN-2：资产处置被驳回 / 撤回 → 资产原地不动（一张凭证都没生成）
    crate::business::erp::asset_bridge::on_disposal_settled(out.instance_id, &out.instance_status).await;
    // FIN-3：报销被驳回 / 撤回 → 回草稿 + 释放预算占用（可改后重提）
    crate::business::erp::expense_bridge::on_expense_settled(out.instance_id, &out.instance_status).await;
    // HR-27：请假被驳回 / 驳回终止 → 回草稿（意见进状态日志，可改后重提）
    crate::business::hr::leave_bridge::on_leave_settled(out.instance_id, &out.instance_status).await;
    // HR-39（ARCH-9 §4.3）：也可能是「薪资单审批」发起的 —— 通过则 SUBMITTED→APPROVED
    // （**不出凭证**，凭证在人工过账时生成）；驳回 / 终止则退回 CALCULATED 重算
    crate::business::hr::salary_bridge::on_salary_settled(out.instance_id, &out.instance_status).await;
    // B3-3：成本结算被驳回 / 驳回终止 → 回草稿 + 清空审批痕迹 + 意见进 remark（可改后重提）
    crate::business::erp::cost_period_bridge::on_cost_settle_decided(out.instance_id, &out.instance_status).await;
    // ARCH-3 R-P1-1：合同审批被驳回 / 驳回终止 → 回草稿（可改后重提，意见进状态日志）
    crate::business::erp::contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;
    // ARCH-3 DC-14：盘点过账审批被驳回 / 终止 → 回草稿（账一分没动，可改实盘数后重提）
    crate::business::erp::stocktake_bridge::on_stock_take_settled(out.instance_id, &out.instance_status).await;
    // P17：付款审批被驳回 / 驳回终止 → 付款记录标 REJECTED（账一分没动，可重新提交）
    crate::business::erp::payable_bridge::on_payment_settled(out.instance_id, &out.instance_status).await;
    // SO-14 遗留 D10：退款审批被驳回 / 终止 → 退款单仍是已确认（账一分没动）
    crate::business::erp::receipt_refund_bridge::on_refund_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3B：应付作废被驳回 → 单据回 OPEN（欠款一分没动，可以再提交）
    crate::business::erp::payable_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
    // P-FLOW-3B：凭证红冲被驳回 → 只清 pending（凭证一直是 POSTED，账没动）
    crate::business::erp::voucher_bridge::on_reverse_settled(out.instance_id, &out.instance_status).await;
    // D3：状态机配置变更被驳回 / 驳回终止 → 配置一分不动（只留审计行）
    crate::business::flow::edge_admin::on_settled(out.instance_id, &out.instance_status).await;
    Ok(Res::ok(action_res(out)))
}

// ==================== 18. 转办 ====================

/// 转办（旧行 `TRANSFERRED` 留痕，新行 `transferred_from` 指向旧行，架构 D12）。
///
/// # 异常情况
///
/// - 400 + `65026`：目标用户不存在 / 已停用 / 是机器人且节点未开 `allow_robot` / 转给自己；
/// - 403 + `65028`：只能转办自己的待办。
#[nik::permission(
    path = "/business/flow/task/{id}/transfer",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/{id}/transfer")]
pub async fn transfer_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::TransferReq>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let task_id = parse_uuid_field(&id, "待办 id")?;
    let to_user_id = parse_uuid_field(&req.to_user_id, "目标用户 id")?;
    let reason = dto::normalize_comment(req.reason.as_deref()).map_err(field_invalid)?;
    let mut out = db()
        .transaction(|tx| async move { engine::transfer(&tx, task_id, operator, to_user_id, reason.as_deref()).await })
        .await
        .map_err(|error| utils::db_error(error, "转办待办"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    Ok(Res::ok(action_res(out)))
}

// ==================== 19. 加签 ====================

/// 并行加签（同节点加人，应到 + 1，架构 D11）。
///
/// # 异常情况
///
/// - 400 + `65027`：`user_ids` 为空 / 超过 10 个 / 没有一个可用；
/// - 409 + `65027`：节点不是审批中；
/// - 403 + `65028`：只能给自己的待办加签。
#[nik::permission(
    path = "/business/flow/task/{id}/add-sign",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/{id}/add-sign")]
pub async fn add_sign_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::AddSignReq>,
) -> ResResult<dto::ActionRes> {
    let operator = me_of(&claims)?;
    let task_id = parse_uuid_field(&id, "待办 id")?;
    if req.user_ids.is_empty() {
        return Err(ApiError::new(400, error_code::FLOW_ADD_SIGN_INVALID, "加签人不能为空"));
    }
    if req.user_ids.len() > dto::MAX_ADD_SIGN_USERS {
        return Err(ApiError::new(
            400,
            error_code::FLOW_ADD_SIGN_INVALID,
            format!("单次加签最多 {} 人", dto::MAX_ADD_SIGN_USERS),
        ));
    }
    let mut user_ids = Vec::with_capacity(req.user_ids.len());
    for raw in &req.user_ids {
        user_ids.push(parse_uuid_field(raw, "加签人 id")?);
    }
    let mut out = db()
        .transaction(|tx| async move { engine::add_sign(&tx, task_id, operator, &user_ids).await })
        .await
        .map_err(|error| utils::db_error(error, "加签"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    Ok(Res::ok(action_res(out)))
}

// ==================== 20. 批量审批 ====================

/// 批量同意 / 驳回（**逐条独立事务**，单条失败不影响其它条，架构 §10.1.3 #28）。
///
/// # 为什么这一层只是「薄壳」（P1-FIX-4 的修复点）
///
/// 批量与单条必须**同一套语义**：内控闸（制单人 ≠ 审批人）、审批事务、以及**落终态之后的
/// 全部业务回调**（任务 / 调拨 / 收货作废 / 收货过账 / 采购 / 预算 / 资产 / 报销 / 合同 /
/// 盘点 / 应付付款 / 应付作废 / 凭证红冲 / 状态机配置）。
///
/// 本函数原来自己写了一套循环，只 `drain` 引擎副作用就返回 —— 于是「批量同意」一张
/// `FLOW_EDGE_CFG` 单，审批单落 `APPROVED` 但 `biz_flow_state_edge` 一行不写
/// （配置一分没生效），另外 13 个业务回调与 9 道内控闸也一起被绕过。
/// 现在改成**逐条委托** [`crate::business::flow::batch::batch_approve`]（它逐条调
/// `approve_task` / `reject_task` 本体），批量与单条不可能再分叉。
///
/// 响应形状（`BatchRes`）与失败码保持老契约：`task_id` / `ok` / `code` / `message`
/// 逐条、按**入参顺序**；非法 UUID 仍然只让那一条失败（不整请求 400）。
///
/// # 异常情况
///
/// - 400 + `65005`：`task_ids` 为空 / 超过 50 条 / 动作非法 / 驳回意见不足 5 字；
/// - 403 + `65004`：驳回并终止 / 退任意节点但缺 `business:flow:admin`。
#[nik::permission(
    path = "/business/flow/task/batch",
    method = "POST",
    permission = "business:flow:approve"
)]
#[nik::post("/business/flow/task/batch")]
pub async fn batch_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::BatchReq>,
) -> ResResult<dto::BatchRes> {
    // 401 兜底：令牌里没有用户标识不该走到批量层
    let _operator = me_of(&claims)?;
    if req.task_ids.is_empty() {
        return Err(field_invalid("task_ids 不能为空"));
    }
    if req.task_ids.len() > dto::MAX_BATCH_TASKS {
        return Err(field_invalid(format!("单次批量审批最多 {} 条", dto::MAX_BATCH_TASKS)));
    }
    let approve = match req.action.trim().to_ascii_uppercase().as_str() {
        "APPROVE" => true,
        "REJECT" => false,
        other => {
            return Err(field_invalid(format!("动作 {other} 非法（只允许 APPROVE / REJECT）")));
        }
    };
    let comment = if approve {
        dto::normalize_comment(req.comment.as_deref()).map_err(field_invalid)?
    } else {
        Some(dto::normalize_reject_comment(req.comment.as_deref().unwrap_or("")).map_err(field_invalid)?)
    };
    let return_to = engine::ReturnTo::parse(req.return_to.as_deref().unwrap_or(""));
    if !approve && return_to.needs_admin() && !claims.has_permission("business:flow:admin") {
        return Err(ApiError::new(
            403,
            error_code::FLOW_FORBIDDEN,
            "批量驳回并终止 / 退任意节点需要流程管理员权限",
        ));
    }

    // ① 主键形态逐条校验（老契约：非法 UUID 只让那一条失败，不整请求 400）
    let mut valid: Vec<String> = Vec::new();
    let mut invalid: Vec<(String, String)> = Vec::new();
    for raw in &req.task_ids {
        let text = raw.trim();
        match Uuid::parse_str(text) {
            Ok(_) => {
                if !valid.iter().any(|item| item == text) {
                    valid.push(text.to_string());
                }
            }
            Err(_) => invalid.push((raw.clone(), "待办 id 格式非法".to_string())),
        }
    }

    // ② 真正的批量：逐条委托单条 handler（内控闸 + 事务 + 全部业务回调都走同一条路）
    let mut results: std::collections::HashMap<String, (bool, i32, String)> = std::collections::HashMap::new();
    if !valid.is_empty() {
        let outcome = crate::business::flow::batch::batch_approve(
            &claims,
            &crate::business::flow::batch::BatchApproveReq {
                ids: valid.clone(),
                decision: Some(if approve { "APPROVE" } else { "REJECT" }.to_string()),
                comment: comment.clone(),
                return_to: req.return_to.clone(),
            },
        )
        .await?;
        for item in &outcome.succeeded {
            results.insert(item.id.clone(), (true, 0, String::new()));
        }
        for item in &outcome.failed {
            results.insert(item.id.clone(), (false, item.code, item.error.clone()));
        }
    }

    // ③ 按入参顺序回填（同一 id 重复出现 → 复用同一条结果，不重复执行）
    let mut items = Vec::with_capacity(req.task_ids.len());
    let mut succeeded = 0usize;
    for raw in &req.task_ids {
        let text = raw.trim();
        let (ok, code, message) = if let Some(hit) = results.get(text) {
            (hit.0, hit.1, hit.2.clone())
        } else if let Some((_, msg)) = invalid.iter().find(|(key, _)| key == raw) {
            (false, error_code::FLOW_FIELD_INVALID, msg.clone())
        } else {
            (false, error_code::FLOW_FIELD_INVALID, "待办 id 格式非法".to_string())
        };
        if ok {
            succeeded += 1;
        }
        items.push(dto::BatchItemRes {
            task_id: raw.clone(),
            ok,
            code,
            message,
        });
    }
    let failed = items.len() - succeeded;
    Ok(Res::ok(dto::BatchRes {
        total: items.len(),
        succeeded,
        failed,
        items,
    }))
}

// ==================== 21–23. 我的待办 / 已办 / 抄送 ====================

/// 待办 / 已办列表行 → 出参（展示字段由 `model::page_my_tasks` 的 JOIN 一次取齐，无 N+1）。
fn my_task_res(row: &model::MyTaskRow) -> dto::TaskRes {
    let initiator_name = row.initiator_name.clone().unwrap_or_default();
    let biz_id = row.biz_id.map(|id| id.to_string()).unwrap_or_default();
    let biz_no = row.biz_no.clone().unwrap_or_default();
    task_res(
        &row.task,
        &TaskCtx {
            instance_no: &row.instance_no,
            title: &row.title,
            biz_type: &row.biz_type,
            biz_id: &biz_id,
            biz_no: &biz_no,
            flow_name: &row.flow_name,
            initiator_name: &initiator_name,
            current_node_name: &row.node_name,
            instance_status: &row.instance_status,
            form_data: Some(&row.form_data),
        },
    )
}

/// 抄送列表行 → 出参。
fn my_cc_res(row: &model::MyCcRow) -> dto::CcRes {
    let initiator_name = row.initiator_name.clone().unwrap_or_default();
    cc_res(
        &row.cc,
        &TaskCtx {
            instance_no: &row.instance_no,
            title: &row.title,
            // 抄送出参（`CcRes`）没有业务类型 / 业务 id / 业务单号，`cc_res` 也不读它们 ——
            // 给空串，不为了「填满结构体」去猜。
            biz_type: "",
            biz_id: "",
            biz_no: "",
            flow_name: &row.flow_name,
            initiator_name: &initiator_name,
            current_node_name: &row.node_name,
            // 抄送出参（`CcRes`）没有实例状态这一列，也不需要（抄送不是待办，点不动的风险只存在于
            // 「待我审批」）；`cc_res` 不读这个字段，这里给空串而不是去猜。
            instance_status: "",
            // 同理：`CcRes` 不下发表单快照（本卡只给待办 / 时间线补 `form_data`）。
            form_data: None,
        },
    )
}

/// 我的待办（**`assignee_id = me AND status = PENDING`**，`APPROVAL` + `RESUBMIT`，
/// 按到达时间倒序）。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/flow/task/todo",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/task/todo")]
pub async fn my_todo(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::MyTaskQuery>,
) -> ResResult<dto::TaskPageRes> {
    let me = me_of(&claims)?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_my_tasks(model::MyTaskQueryParams {
        user_id: me,
        statuses: model::MY_TODO_STATUSES,
        kinds: &dto::TASK_KINDS,
        order: model::MyTaskOrder::Arrived,
        page: page_no,
        page_size,
        // P11：待办只认实例还在跑的单子（历史脏数据不进审批人的列表）
        live_only: true,
        // 待办还没有 `completed_at`，`since` 只对「我的已办」有意义
        completed_since: None,
    })
    .await
    .map_err(|error| utils::db_error(error, "查询我的待办"))?;
    Ok(Res::ok(dto::TaskPageRes {
        list: result.list.iter().map(my_task_res).collect(),
        total: result.total,
        page: page_no,
        page_size,
    }))
}

/// 我的已办（**我操作过的**：`APPROVED` / `REJECTED` / `TRANSFERRED`，按操作时间倒序）。
///
/// 「操作人」口径见 `model::page_my_tasks` 的文档：任务行的 `assignee_id` 即操作人。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/flow/task/done",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/task/done")]
pub async fn my_done(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::MyTaskQuery>,
) -> ResResult<dto::TaskPageRes> {
    let me = me_of(&claims)?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    // 首页工作台的「今日已办」只数今天操作过的：`since` 传了就加 `completed_at >= since`。
    // 非法时间 → 400，不静默忽略（静默忽略会让卡片显示全量历史，比报错更难发现）。
    let since = match query.since.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(parse_since_field(raw, "since")?),
        _ => None,
    };
    let result = model::page_my_tasks(model::MyTaskQueryParams {
        user_id: me,
        statuses: model::MY_DONE_STATUSES,
        kinds: &dto::TASK_KINDS,
        order: model::MyTaskOrder::Operated,
        page: page_no,
        page_size,
        // 已办是历史记录：实例结束与否都要显示（不受 P11 的「只看在跑」约束）
        live_only: false,
        completed_since: since,
    })
    .await
    .map_err(|error| utils::db_error(error, "查询我的已办"))?;
    Ok(Res::ok(dto::TaskPageRes {
        list: result.list.iter().map(my_task_res).collect(),
        total: result.total,
        page: page_no,
        page_size,
    }))
}

/// 抄送我的（**`biz_flow_cc.receiver_id = me`**，按抄送时间倒序）。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/flow/task/cc",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/task/cc")]
pub async fn my_cc(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::MyTaskQuery>,
) -> ResResult<dto::CcPageRes> {
    let me = me_of(&claims)?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_my_cc(me, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询抄送我的"))?;
    Ok(Res::ok(dto::CcPageRes {
        list: result.list.iter().map(my_cc_res).collect(),
        total: result.total,
        page: page_no,
        page_size,
    }))
}

// ==================== 24. 我发起的 ====================

/// 我发起的审批单（**`initiator_id = me`**，按发起时间倒序）。
///
/// D5 缺口 3：D2 只给了 `/instance/page`（可见性并集），「我发起」Tab 没有稳定的
/// 身份口径列表 —— 这里补一条只按发起人过滤的列表（数据范围/抄送/待办身份都不掺）。
/// 静态段 `/my` 与 `/instance/{id}` 共存时 matchit 静态优先，已实测可达。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/flow/instance/my",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/my")]
pub async fn my_instances(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::MyTaskQuery>,
) -> ResResult<dto::InstancePageRes> {
    let me = me_of(&claims)?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_my_instances(me, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询我发起的审批单"))?;
    let mut list = Vec::with_capacity(result.list.len());
    for row in &result.list {
        let my_task = model::my_task_of(row.id, me)
            .await
            .map_err(|error| utils::db_error(error, "查询我的待办"))?;
        list.push(instance_res(row, my_task.map(|task| task.id)));
    }
    Ok(Res::ok(dto::InstancePageRes {
        list,
        total: result.total,
        page: result.page,
        page_size: result.size,
    }))
}

// ==================== 统计（D4b：总览 / 效率 / 卡点） ====================

/// 统计查询参数（`start` / `end` 均可省略，缺省近 30 天）。
///
/// 时间格式：`YYYY-MM-DD`（按 UTC 零点，左闭）或 RFC3339（右开）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StatsQuery {
    /// 起始时间（含）。
    #[serde(default)]
    pub start: Option<String>,
    /// 结束时间（不含）。
    #[serde(default)]
    pub end: Option<String>,
}

/// 总览响应（**无数据一律 0，绝不下发 null**）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsOverviewRes {
    /// 时间窗起点。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 时间窗终点。
    pub end: chrono::DateTime<chrono::Utc>,
    /// 发起数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数。
    pub pending: i64,
    /// 撤回数。
    pub cancelled: i64,
    /// 发起人撤回数。
    pub withdrawn: i64,
    /// 已结束数。
    pub finished: i64,
    /// 通过率（0–1，4 位小数）。
    pub approved_rate: f64,
    /// 平均审批时长（秒）。
    pub avg_duration_seconds: i64,
}

/// 单流程效率行。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlowEfficiencyRes {
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 实例数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数。
    pub running: i64,
    /// 已结束数。
    pub finished: i64,
    /// 平均审批时长（秒）。
    pub avg_duration_seconds: i64,
    /// 最长单实例时长（秒）。
    pub max_duration_seconds: i64,
}

/// 流程效率响应。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsEfficiencyRes {
    /// 时间窗起点。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 时间窗终点。
    pub end: chrono::DateTime<chrono::Utc>,
    /// 各流程效率（按总耗时降序）。
    pub list: Vec<FlowEfficiencyRes>,
}

/// 单节点卡点行。
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeBottleneckRes {
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 样本数。
    pub samples: i64,
    /// 平均停留（秒）。
    pub avg_dwell_seconds: i64,
    /// 最长停留（秒）。
    pub max_dwell_seconds: i64,
}

/// 卡点响应（`slowest_*` 无数据时为**空串 / 空数组**，不是 null）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsBottleneckRes {
    /// 时间窗起点。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 时间窗终点。
    pub end: chrono::DateTime<chrono::Utc>,
    /// 最慢节点 key（无数据 → 空串）。
    pub slowest_node_key: String,
    /// 最慢节点名（无数据 → 空串）。
    pub slowest_node_name: String,
    /// 最慢节点平均停留（秒；无数据 → 0）。
    pub slowest_avg_dwell_seconds: i64,
    /// 卡点榜。
    pub list: Vec<NodeBottleneckRes>,
}

/// 解析统计时间参数（`YYYY-MM-DD` → UTC 零点；否则按 RFC3339）。
fn parse_stat_time(
    raw: Option<&str>,
    label: &str,
) -> std::result::Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    match raw.map(str::trim) {
        None => Ok(None),
        Some("") => Ok(None),
        Some(value) => {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                && let Some(local) = date.and_hms_opt(0, 0, 0)
            {
                return Ok(Some(local.and_utc()));
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
                return Ok(Some(dt.with_timezone(&chrono::Utc)));
            }
            Err(field_invalid(format!(
                "{label} 时间格式非法（只允许 YYYY-MM-DD 或 RFC3339）：{value}"
            )))
        }
    }
}

/// 归一化时间窗（缺省近 30 天；`start < end`；跨度 ≤ 366 天）。
fn stats_window(query: &StatsQuery) -> std::result::Result<model::StatsWindow, ApiError> {
    let start = parse_stat_time(query.start.as_deref(), "start")?;
    let end = parse_stat_time(query.end.as_deref(), "end")?;
    model::StatsWindow::normalize(start, end, chrono::Utc::now()).map_err(field_invalid)
}

/// 审批总览（发起数 / 通过数 / 驳回数 / 平均时长）。
///
/// 可见性：**只统计当前用户可见的实例**（复用列表接口的并集口径）。
///
/// # 异常情况
///
/// - 400 + `65005`：时间格式非法 / `start >= end` / 跨度超限；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/stats/overview",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/stats/overview")]
pub async fn stats_overview(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<StatsQuery>,
) -> ResResult<StatsOverviewRes> {
    me_of(&claims)?;
    let window = stats_window(&query)?;
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let rows = model::efficiency_rows(&principal, window)
        .await
        .map_err(|error| utils::db_error(error, "统计审批总览"))?;
    let overview = model::fold_overview(&rows);
    Ok(Res::ok(StatsOverviewRes {
        start: window.start,
        end: window.end,
        total: overview.total,
        approved: overview.approved,
        rejected: overview.rejected,
        pending: overview.running,
        cancelled: overview.cancelled,
        withdrawn: overview.withdrawn,
        finished: overview.finished,
        approved_rate: overview.approved_rate(),
        avg_duration_seconds: overview.avg_duration_seconds(),
    }))
}

/// 流程效率（各流程平均审批时长 = 实例结束时间 − 发起时间）。
///
/// # 异常情况
///
/// - 400 + `65005`：时间格式非法 / 范围非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/stats/efficiency",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/stats/efficiency")]
pub async fn stats_efficiency(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<StatsQuery>,
) -> ResResult<StatsEfficiencyRes> {
    me_of(&claims)?;
    let window = stats_window(&query)?;
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let rows = model::efficiency_rows(&principal, window)
        .await
        .map_err(|error| utils::db_error(error, "统计流程效率"))?;
    let list = rows
        .iter()
        .map(|row| FlowEfficiencyRes {
            flow_code: row.flow_code.clone(),
            flow_name: row.flow_name.clone(),
            total: row.total,
            approved: row.approved,
            rejected: row.rejected,
            running: row.running,
            finished: row.finished,
            avg_duration_seconds: row.avg_duration_seconds(),
            max_duration_seconds: row.max_duration_seconds,
        })
        .collect();
    Ok(Res::ok(StatsEfficiencyRes {
        start: window.start,
        end: window.end,
        list,
    }))
}

/// 卡点（哪个节点平均停留最久）。
///
/// # 异常情况
///
/// - 400 + `65005`：时间格式非法 / 范围非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/stats/node-bottleneck",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/stats/node-bottleneck")]
pub async fn stats_node_bottleneck(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<StatsQuery>,
) -> ResResult<StatsBottleneckRes> {
    me_of(&claims)?;
    let window = stats_window(&query)?;
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let rows = model::bottleneck_rows(&principal, window, model::BOTTLENECK_TOP)
        .await
        .map_err(|error| utils::db_error(error, "统计流程卡点"))?;
    let slowest = model::slowest_of(&rows);
    let list = rows
        .iter()
        .map(|row| NodeBottleneckRes {
            node_key: row.node_key.clone(),
            node_name: row.node_name.clone(),
            samples: row.samples,
            avg_dwell_seconds: row.avg_dwell_seconds,
            max_dwell_seconds: row.max_dwell_seconds,
        })
        .collect();
    Ok(Res::ok(StatsBottleneckRes {
        start: window.start,
        end: window.end,
        slowest_node_key: slowest.map(|row| row.node_key.clone()).unwrap_or_default(),
        slowest_node_name: slowest.map(|row| row.node_name.clone()).unwrap_or_default(),
        slowest_avg_dwell_seconds: slowest.map(|row| row.avg_dwell_seconds).unwrap_or(0),
        list,
    }))
}

// ==================== 25b. 自审监控（P3-05） ====================

/// 自审监控查询参数。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SelfApprovalQuery {
    /// 月份（`YYYY-MM`；缺省 / 空串 = 当月，**按北京时间自然月**切分）。
    #[serde(default)]
    pub month: Option<String>,
    /// 业务类型过滤（缺省 = 全部业务）。
    #[serde(default)]
    pub biz_type: Option<String>,
}

/// 按节点聚合（自审监控）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfApprovalNodeRes {
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 自审票数。
    pub tickets: i64,
    /// 涉及审批单数。
    pub instances: i64,
    /// 涉及金额合计（**字符串**：金额一律 `::text`，别用浮点加减）。
    pub amount: String,
}

/// 按人聚合（自审监控）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfApprovalPersonRes {
    /// 审批人 id（字符串：`Uuid` 不实现 `Serialize`，DTO 一律转 string）。
    pub user_id: String,
    /// 审批人姓名。
    pub assignee_name: String,
    /// 自审票数。
    pub tickets: i64,
    /// 涉及审批单数。
    pub instances: i64,
    /// 涉及金额合计（字符串）。
    pub amount: String,
}

/// 明细（自审监控）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfApprovalTicketRes {
    /// 待办 id（字符串）。
    pub task_id: String,
    /// 审批单 id（字符串）。
    pub instance_id: String,
    /// 单据号。
    pub instance_no: String,
    /// 标题。
    pub title: String,
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 审批人 id（字符串）。
    pub user_id: String,
    /// 审批人姓名。
    pub assignee_name: String,
    /// 投票时刻。
    pub voted_at: chrono::DateTime<chrono::Utc>,
    /// 金额（字符串）。
    pub amount: String,
}

/// 自审监控响应。
///
/// **口径全部写在这里**（分子 / 分母 / 金额单位），前端不再自己解释：
///
/// - `self_approval_instances`（分子）= 本窗内有**自审票被通过**的审批单数（去重）；
/// - `settled_instances`（分母）= 同窗**已结算**（`status=APPROVED` 且 `ended_at` 落在本窗）的审批单数；
/// - `ratio` = 分子 / 分母（0–1，保留 4 位；分母 0 → 0.0，绝不下发 NaN）；
/// - `self_approval_amount` = 分子的那些单据的 `form_data` 金额合计（字符串，避免浮点误差）；
/// - `blocked_count` = 本窗**已生效**的「超自审金额上限被降级」次数（数 `operation_logs`，
///   被 65044 整单拒绝的那次因事务回滚不在此列，见 `blocked_note`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfApprovalRes {
    /// 月份（`YYYY-MM`）。
    pub month: String,
    /// 时间窗起点（含，UTC）。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 时间窗终点（不含，UTC）。
    pub end: chrono::DateTime<chrono::Utc>,
    /// 当前生效的自审金额上限（0 = 不限额）。
    pub threshold: f64,
    /// 分母：同期已结算审批单数。
    pub settled_instances: i64,
    /// 分子：自审单数。
    pub self_approval_instances: i64,
    /// 自审票数。
    pub self_approval_tickets: i64,
    /// 自审金额合计（字符串）。
    pub self_approval_amount: String,
    /// 占比（分子 / 分母，0–1）。
    pub ratio: f64,
    /// 占比口径说明（人话，直接展示）。
    pub ratio_note: String,
    /// 超阈值被降级次数。
    pub blocked_count: i64,
    /// 拦截计数口径说明。
    pub blocked_note: String,
    /// 按节点 top N。
    pub by_node: Vec<SelfApprovalNodeRes>,
    /// 按人 top N。
    pub by_person: Vec<SelfApprovalPersonRes>,
    /// 明细（最近 50 条）。
    pub list: Vec<SelfApprovalTicketRes>,
}

/// 月度自审监控（`GET /business/flow/stats/self-approval`，P3-05）。
///
/// # 权限码
///
/// 复用 `business:flow:instance:read` —— 与另外三个统计接口（overview / efficiency /
/// node-bottleneck）**同一口径**，也是审批实例列表的读权限。刻意**不新造**
/// `business:flow:stats:read`：架构 §10.2 规划过这个码但从未落库，「新增一个只给这一个
/// 接口用的码」会让权限页多出一行管理员不敢勾的死码（N14/N18 门禁同源问题）。
///
/// # 可见性
///
/// 与实例列表 / 其余统计接口**同一份** `instance_visibility()`（数据范围 ∪ 我发起的 ∪
/// 我待办/已办）。非超管看不到别的部门的自审数据。
///
/// # 异常情况
///
/// - `400 + 65005`：`month` 不是 `YYYY-MM`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/stats/self-approval",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/stats/self-approval")]
pub async fn stats_self_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<SelfApprovalQuery>,
) -> ResResult<SelfApprovalRes> {
    me_of(&claims)?;
    let window = self_approval::Window::parse(query.month.as_deref(), chrono::Utc::now()).map_err(field_invalid)?;
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let threshold = crate::business::erp::separation::self_approval_max_amount().await;
    let biz_type = query
        .biz_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let report = self_approval::monthly_report(&principal, &window, biz_type, threshold)
        .await
        .map_err(|error| utils::db_error(error, "统计自审监控"))?;
    // 先算占比（`ratio()` 借用 report，部分字段 move 之后就不能再借了）
    let ratio = report.ratio();

    Ok(Res::ok(SelfApprovalRes {
        month: report.month,
        start: report.start,
        end: report.end,
        threshold: report.threshold,
        settled_instances: report.settled_instances,
        self_approval_instances: report.self_approval_instances,
        self_approval_tickets: report.self_approval_tickets,
        self_approval_amount: report.self_approval_amount,
        ratio,
        ratio_note: self_approval::RATIO_NOTE.to_string(),
        blocked_count: report.blocked_count,
        blocked_note: "只统计**已生效**的降级（实例仍在，按可见性归属）；\
                       被 65044 整单拒绝的提交因事务回滚不在计数内，\
                       会在报错报文与服务端 WARN 日志留痕。"
            .to_string(),
        by_node: report
            .by_node
            .iter()
            .map(|row| SelfApprovalNodeRes {
                node_key: row.node_key.clone(),
                node_name: row.node_name.clone(),
                tickets: row.tickets,
                instances: row.instances,
                amount: row.amount.clone(),
            })
            .collect(),
        by_person: report
            .by_person
            .iter()
            .map(|row| SelfApprovalPersonRes {
                user_id: row.user_id.to_string(),
                assignee_name: row.assignee_name.clone(),
                tickets: row.tickets,
                instances: row.instances,
                amount: row.amount.clone(),
            })
            .collect(),
        list: report
            .list
            .iter()
            .map(|row| SelfApprovalTicketRes {
                task_id: row.task_id.to_string(),
                instance_id: row.instance_id.to_string(),
                instance_no: row.instance_no.clone(),
                title: row.title.clone(),
                node_key: row.node_key.clone(),
                node_name: row.node_name.clone(),
                user_id: row.user_id.to_string(),
                assignee_name: row.assignee_name.clone(),
                voted_at: row.voted_at,
                amount: row.amount.clone(),
            })
            .collect(),
    }))
}

// ==================== 26. 任务节点预演（P-FLOW-1A） ====================

/// 任务节点模板预演（`POST /business/flow/node/task/preview`，设计 §3.3）。
///
/// **不发布、不建实例**：拿一份 `config.task` + 一份模拟变量，看渲染出的任务长什么样。
///
/// # 权限为什么是 `business:flow:tasknode:read` 而不是设计 §3.3 写的 `business:flow:read`
///
/// §3.8 专门为「查看流程任务配置」造了 `business:flow:tasknode:read`，而 1A 只有这一个新接口。
/// 用 `flow:read` 会让那个码**没有任何接口** —— 直接命中 `system::menu::permission_audit_tests`
/// 的 N14/N18（菜单里有码、后端零落地 = 管理员勾了永不生效的死码）。
/// `tasknode:read` 的语义（「能看任务配置就能预演」）与预演接口完全吻合，且当前没有前端调用方
/// 依赖 `flow:read` 这一路径（页面在 P-FLOW-1C 才建）。详见 `V0181` 迁移头部的取舍说明。
///
/// # `strict` 的两种模式（设计 §3.3「缺变量处理」）
///
/// | `strict` | 缺变量 | 用途 |
/// |---|---|---|
/// | `false`（默认） | 渲染成可见占位 `〔form.amount〕` + `warnings` | 配置期预览（用户还没数据） |
/// | `true` | 与运行期一致：`400 + 65039` | 「上线前体检」按钮 |
///
/// `strict=false` **不是静默**：占位是显式的、warning 是逐条给的。真实落库永远走严格模式。
///
/// # 异常情况
///
/// - `400 + 65034`：`config` 形状 / 未知键 / 交叉约束非法；
/// - `400 + 65035`：`need_review × on_complete` 矩阵违例（含 1A 阶段的 APPROVE 闸）；
/// - `400 + 65036`：模板语法错（占位符未闭合 / 变量名非法 / 超过一层点号）；
/// - `400 + 65037`：变量路径不在白名单 / `task.*` 用在非 `TASK_ACCEPT`；
/// - `400 + 65039`：`strict=true` 缺变量 / 渲染后标题超 200 字符；
/// - `400 + 65018`：指派人解析出 0 人且 `empty_assignee=ERROR`。
#[nik::permission(
    path = "/business/flow/node/task/preview",
    method = "POST",
    permission = "business:flow:tasknode:read"
)]
#[nik::post("/business/flow/node/task/preview")]
pub async fn task_preview(Json(req): Json<dto::TaskPreviewReq>) -> ResResult<dto::TaskPreviewRes> {
    let strict = req.strict.unwrap_or(false);

    // ① 形状 + 交叉约束（65034）
    let cfg = node_config::parse_task_config(&req.config)?;
    // ② 矩阵（65035）
    node_config::check_matrix(&cfg)?;

    // ③ 变量表：请求给的模拟变量 + `{{node.name}}`
    let mut source = match req.vars.clone() {
        Some(Value::Object(map)) => Value::Object(map),
        _ => Value::Object(Map::new()),
    };
    if let Some(name) = req.node_name.as_deref() {
        let node = source
            .as_object_mut()
            .map(|map| map.entry("node").or_insert_with(|| Value::Object(Map::new())));
        if let Some(node) = node.and_then(Value::as_object_mut) {
            node.insert("name".to_string(), Value::String(name.to_string()));
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    let vars = template_vars::TemplateVars::from_nested(&source, &mut warnings);

    // ④ R21：变量路径白名单（65037）+ 模板语法（65036）
    let task_accept = req
        .biz_type
        .as_deref()
        .map(|raw| raw.trim().eq_ignore_ascii_case("TASK_ACCEPT"))
        .unwrap_or(false);
    for (label, template) in [
        ("title_template", Some(cfg.title_template.as_str())),
        ("description_template", cfg.description_template.as_deref()),
    ] {
        let Some(template) = template else {
            continue;
        };
        let placeholders = render::collect_placeholders_paths(template)?;
        for name in placeholders {
            if !graph::template_field_allowed(&name) {
                return Err(ApiError::new(
                    400,
                    error_code::FLOW_TEMPLATE_VAR_INVALID,
                    format!(
                        "{label} 里的 {{{{{name}}}}} 不在白名单（允许 form.* / instance.* / initiator.* / node.* / prev.* / task.*）"
                    ),
                ));
            }
            if name.starts_with("task.") && !task_accept {
                return Err(ApiError::new(
                    400,
                    error_code::FLOW_TEMPLATE_VAR_INVALID,
                    format!(
                        "{label} 用了 {{{{{name}}}}}，但本流程业务类型是 {}（task.* 只在 TASK_ACCEPT 流程里有值）",
                        req.biz_type.as_deref().unwrap_or("（未知）")
                    ),
                ));
            }
        }
    }

    // ⑤ 渲染：严格模式与运行期逐字节一致；非严格模式把缺变量换成可见占位
    let dotted = if strict {
        vars.dotted.clone()
    } else {
        let templates = [Some(cfg.title_template.as_str()), cfg.description_template.as_deref()];
        let present: Vec<&str> = templates.into_iter().flatten().collect();
        let (filled, missing) = template_vars::fill_missing_with_placeholders(&vars.dotted, &present);
        for path in missing {
            warnings.push(format!(
                "缺变量 {{{{{path}}}}}：已渲染成可见占位（严格模式下会 400 + 65039）"
            ));
        }
        filled
    };

    let title = render::render_paths(&cfg.title_template, &dotted, ContentFormat::Text).map_err(preview_error)?;
    let title_chars = title.chars().count();
    if title_chars > node_config::MAX_TITLE_TEMPLATE_CHARS {
        return Err(ApiError::new(
            400,
            error_code::FLOW_PREVIEW_INVALID,
            format!(
                "渲染后标题 {title_chars} 字符，超过 {}（biz_task.title 上限）",
                node_config::MAX_TITLE_TEMPLATE_CHARS
            ),
        ));
    }
    let description = match cfg.description_template.as_deref() {
        Some(template) => render::render_paths(template, &dotted, ContentFormat::Text).map_err(preview_error)?,
        None => String::new(),
    };

    // ⑥ 期限（`due_base` 可注入，便于前端「假设今天」）
    // 预演的「今天」由 `due_base` 注入（前端要能演「假设今天」）；表单取 `vars.form`
    let form_only = source.get("form").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let due_date = cfg.due.as_ref().map(|due| {
        let base = req.due_base.unwrap_or_else(chrono::Utc::now);
        task_node::compute_due(due, &form_only, base)
    });

    // ⑦ 指派（可选）：与运行期走**同一个** `assignee::resolve`
    let mut assignees: Vec<dto::PreviewAssigneeRes> = Vec::new();
    let mut trace: Vec<String> = Vec::new();
    if let Some(assignee) = req.assignee.as_ref() {
        let spec = preview_snapshot_node(assignee).await?;
        let ctx = assignee::InstanceCtx {
            instance_id: Uuid::nil(),
            initiator_id: Uuid::nil(),
            initiator_dept_id: None,
            biz_type: req.biz_type.clone().unwrap_or_default(),
            biz_id: None,
            form_data: source.clone(),
        };
        let eval = preview_eval_context(&source);
        let assignee::Resolution {
            finalized,
            trace: resolved,
        } = assignee::resolve(db(), &spec, &ctx, &eval)
            .await
            .map_err(|error| utils::db_error(error, "预演解析指派人"))?;
        trace = resolved;
        if let Some(error) = finalized.error {
            return Err(error);
        }
        for (index, candidate) in finalized.approvers.iter().enumerate() {
            assignees.push(dto::PreviewAssigneeRes {
                user_id: candidate.user_id.to_string(),
                name: candidate.name.clone(),
                source: candidate.source.as_str().to_string(),
                source_ref: candidate.source_ref.map(|id| id.to_string()).unwrap_or_default(),
                is_robot: candidate.is_robot,
                member: index > 0,
            });
        }
        if assignees.is_empty() {
            warnings.push("按当前指派配置解析出 0 个负责人（运行期会按 empty_assignee 策略处理）".to_string());
        }
    }

    Ok(Res::ok(dto::TaskPreviewRes {
        ok: true,
        task: Some(dto::PreviewTaskRes {
            title,
            description,
            priority: cfg.priority.clone(),
            due_date: due_date.map(|date| date.format("%Y-%m-%d").to_string()),
            need_review: cfg.need_review,
            on_complete: cfg.on_complete.as_str().to_string(),
        }),
        assignees,
        warnings,
        errors: Vec::new(),
        trace,
    }))
}

/// 预演里的渲染错误映射（设计 §3.3 的错误表）。
///
/// - 语法错（60024：未闭合 / 变量名非法 / 超过一层点号）→ `65036`；
/// - 缺变量（60023）→ `65039`（`strict=true` 时才会出现；`strict=false` 已被占位填掉）。
fn preview_error(error: ApiError) -> ApiError {
    let code = match error.code {
        error_code::NOTICE_TEMPLATE_SYNTAX => error_code::FLOW_NOTIFY_TEMPLATE_INVALID,
        _ => error_code::FLOW_PREVIEW_INVALID,
    };
    ApiError::new(400, code, error.msg)
}

/// 把预演请求里的 `assignee` 变成 `SnapshotNode`（与发布快照同构，才能复用 `assignee::resolve`）。
async fn preview_snapshot_node(
    req: &dto::PreviewAssigneeReq,
) -> std::result::Result<crate::business::flow::graph::SnapshotNode, ApiError> {
    let mut spec = crate::business::flow::graph::SnapshotNode {
        key: "n_preview".to_string(),
        name: "预演节点".to_string(),
        node_type: NodeType::Task.as_str().to_string(),
        assignee_type: req.assignee_type.clone(),
        approval_mode: None,
        approve_ratio: None,
        reject_veto: true,
        join_mode: "AND".to_string(),
        fork_mode: "AND".to_string(),
        form_var_key: req.form_var_key.clone(),
        superior_levels: req.superior_levels.unwrap_or(1),
        empty_assignee: req.empty_assignee.clone().unwrap_or_else(|| "ERROR".to_string()),
        // 任务节点没有「自审」这个概念（设计 §2.4）：预演必须与运行期同口径，
        // 否则会出现「预演 1 人、真跑 0 人」的假绿（运行期 `task_node::spawn` 同样置 ALLOW）。
        self_approval: "ALLOW".to_string(),
        allow_robot: req.allow_robot.unwrap_or(false),
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
        config: Value::Object(Map::new()),
        assignees: Vec::new(),
    };
    // USER：`user_ids` 直接转 UUID
    for raw in &req.user_ids {
        let id = parse_uuid_field(raw, "用户 id")?;
        spec.assignees.push(crate::business::flow::graph::SnapshotAssignee {
            assignee_type: AssigneeKind::User.as_str().to_string(),
            ref_id: id.to_string(),
            sort_order: spec.assignees.len() as i32,
        });
    }
    // ROLE：`role_codes` 走与表达式审批人同一个 code→id 解析
    if !req.role_codes.is_empty() {
        let mut notes: Vec<String> = Vec::new();
        let ids = assignee::load_role_ids_by_code(db(), &req.role_codes, &mut notes)
            .await
            .map_err(|error| utils::db_error(error, "解析角色编码"))?;
        for id in ids {
            spec.assignees.push(crate::business::flow::graph::SnapshotAssignee {
                assignee_type: AssigneeKind::Role.as_str().to_string(),
                ref_id: id.to_string(),
                sort_order: spec.assignees.len() as i32,
            });
        }
    }
    Ok(spec)
}

/// 预演用的求值上下文（`form` / `initiator` / `instance` / `task` 四棵树，与运行期同构）。
fn preview_eval_context(source: &Value) -> graph::EvalContext {
    let pick = |root: &str| source.get(root).and_then(Value::as_object).cloned().unwrap_or_default();
    graph::EvalContext {
        form: pick("form"),
        initiator: pick("initiator"),
        instance: pick("instance"),
        task: pick("task"),
    }
}

// ==================== 27. 审批单下的流程任务 ====================

/// 本审批单由 `TASK_NODE` 生成的任务列表（`GET /business/flow/instance/{id}/tasks`，设计 §3.2）。
///
/// 可见性口径与 [`instance_detail`] **完全一致**：发起人 / 审批人（含已审）/ 抄送人 / 流程管理员。
/// 不可见一律 403（不暴露「存在但无权」）。
///
/// # 异常情况
///
/// - 400 + `65005`：id 不是 UUID；
/// - 404 + `65002`：审批单不存在；
/// - 403 + `65004`：与本单无关。
#[nik::permission(
    path = "/business/flow/instance/{id}/tasks",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/{id}/tasks")]
pub async fn instance_tasks(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::InstanceTasksRes> {
    let me = me_of(&claims)?;
    let instance_id = parse_uuid_field(&id, "审批单 id")?;
    let row = model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .ok_or_else(|| ApiError::new(404, error_code::FLOW_INSTANCE_NOT_FOUND, "审批单不存在"))?;

    let mine = model::my_instance_ids(me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的相关审批单"))?;
    let allowed = row.initiator_id == me || mine.contains(&row.id) || claims.has_permission("business:flow:admin");
    if !allowed {
        return Err(ApiError::new(
            403,
            error_code::FLOW_FORBIDDEN,
            "你与这张审批单无关，不能查看",
        ));
    }

    let rows = model::flow_tasks_of(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单生成的任务"))?;
    let list: Vec<dto::InstanceTaskRes> = rows
        .iter()
        .map(|task| dto::InstanceTaskRes {
            id: task.id.to_string(),
            task_no: task.task_no.clone(),
            title: task.title.clone(),
            status: task.status.clone(),
            priority: task.priority.clone(),
            progress: task.progress,
            owner_id: task.owner_id.to_string(),
            owner_name: task.owner_name.clone(),
            need_review: task.need_review,
            due_date: task.due_date,
            flow_node_key: task.flow_node_key.clone(),
            flow_round: task.flow_round,
            created_at: task.created_at,
        })
        .collect();
    let total = list.len() as i64;
    Ok(Res::ok(dto::InstanceTasksRes {
        instance_id: instance_id.to_string(),
        list,
        total,
    }))
}

// ==================== 标准流程列表（P-FLOW-5，P2-6b 语义收口） ====================

/// 分类展示顺序（= 前端 Tab 顺序，与卡面「采购管理 / 仓库管理 / 财务管理 / 任务」一致）。
const TEMPLATE_CATEGORY_ORDER: [&str; 4] = ["采购管理", "仓库管理", "财务管理", "任务"];

/// 分类排序权重：已知业务域按固定顺序，未知分类排最后（按名字排）。
fn template_category_rank(category: &str) -> usize {
    TEMPLATE_CATEGORY_ORDER
        .iter()
        .position(|known| *known == category)
        .unwrap_or(TEMPLATE_CATEGORY_ORDER.len())
}

/// 截断到 `max` 个字符（按字符不是字节，中文不会被切一半）。
fn truncate_chars(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// 模板卡片的一句话适用场景。
///
/// 优先用**条件连线的分流标签**（「金额 < 10000 / 10000 ≤ 金额 < 100000 / 金额 ≥ 100000」）——
/// 这是真正决定审批走向的阈值，比说明文字更接近用户问的「这个流程适用于什么情况」；
/// 没有条件边（如任务验收）才退到 `description` 的首句。
fn template_summary(labels: &[String], description: Option<&str>) -> String {
    if !labels.is_empty() {
        return truncate_chars(&labels.join(" / "), 120);
    }
    let Some(desc) = description.map(str::trim).filter(|text| !text.is_empty()) else {
        return String::new();
    };
    let first = desc.split(['。', '\n']).next().unwrap_or(desc).trim();
    truncate_chars(first, 120)
}

/// 解析标准流程列表口径（`scope` 优先，legacy `include_mine` 兜底）。
///
/// - `scope` 非空 → 只认它（`standard` / `all`，大小写不敏感），非法值 → 400 + `65005`；
/// - `scope` 缺省 / 空串 → 看 legacy `include_mine`：真值 ⇒ `all`，其余 ⇒ `standard`。
fn parse_template_scope(
    scope: Option<&str>,
    include_mine: Option<&str>,
) -> std::result::Result<model::TemplateScope, ApiError> {
    if let Some(raw) = scope.map(str::trim).filter(|text| !text.is_empty()) {
        return match raw.to_ascii_lowercase().as_str() {
            "standard" => Ok(model::TemplateScope::Standard),
            "all" => Ok(model::TemplateScope::All),
            _ => Err(ApiError::new(
                400,
                error_code::FLOW_FIELD_INVALID,
                format!("scope {raw} 非法（只允许 standard / all）"),
            )),
        };
    }
    if query_truthy(include_mine) {
        Ok(model::TemplateScope::All)
    } else {
        Ok(model::TemplateScope::Standard)
    }
}

/// 查询参数里的「真值」判断（`1` / `true` / `yes` / `on`，大小写不敏感；其余一律视为假）。
fn query_truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// 标准流程列表（`GET /business/flow/def/templates`）。
///
/// **legacy 别名：语义已收口为「标准流程」列表**（端点名保留不改 —— 改名收益 < 动 5 个文件的风险）。
///
/// 默认只返回**标准流程**（`is_system = TRUE`，即「模板」这个角色，不是独立实体，决策 A）；
/// `?scope=all` 才把客户另存为的副本一起带出，老前端的 `?include_mine=1` 等价于 `scope=all`。
///
/// 一次返回**按业务域分组**的条目，每组内标准流程在前，每条带「标准」标记（`is_system`）、
/// 启用状态、节点 / 连线数、已用次数与一句话适用场景。
///
/// **不按 `status` 过滤**：标准流程被客户停用（`DISABLED`）后仍要出现在列表里，
/// 否则它就从管理视图消失、再也启用不回来；`status` 只给前端做标签展示。
///
/// 数据来源全部是库里的真实事实：分组 = `biz_flow_def.category`，
/// 已用次数 = `biz_flow_instance` 计数，适用场景 = 条件连线标签（退 `description` 首句）。
///
/// # 异常情况
///
/// - 400 + `65005`：`scope` 取值非法（只允许 `standard` / `all`）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/def/templates",
    method = "GET",
    permission = "business:flow:read"
)]
#[nik::get("/business/flow/def/templates")]
pub async fn templates(Query(query): Query<dto::TemplateQuery>) -> ResResult<dto::TemplatesRes> {
    let scope = parse_template_scope(query.scope.as_deref(), query.include_mine.as_deref())?;
    let rows = model::templates(scope)
        .await
        .map_err(|error| utils::db_error(error, "查询标准流程列表"))?;
    let labels = model::template_condition_labels()
        .await
        .map_err(|error| utils::db_error(error, "查询流程分流标签"))?;

    let mut labels_of: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (def_id, label) in labels {
        labels_of.entry(def_id).or_default().push(label);
    }

    // 分组（保持 SQL 的组内顺序：系统预置优先 → 分类 → 编码）
    let mut groups: Vec<dto::TemplateGroupRes> = Vec::new();
    for row in &rows {
        let category = row.category_label();
        let label_list = labels_of.remove(&row.id).unwrap_or_default();
        let item = dto::TemplateItemRes {
            id: row.id.to_string(),
            flow_code: row.flow_code.clone(),
            flow_name: row.flow_name.clone(),
            biz_type: row.biz_type.clone(),
            category: category.clone(),
            status: row.status.clone(),
            is_default: row.is_default,
            is_system: row.is_system,
            view_version_id: row.view_version_id.map(|id| id.to_string()),
            view_version_no: row.view_version_no,
            node_count: row.node_count,
            edge_count: row.edge_count,
            usage_count: row.usage_count,
            last_used_at: row.last_used_at,
            summary: template_summary(&label_list, row.description.as_deref()),
        };
        match groups.iter_mut().find(|group| group.category == category) {
            Some(group) => group.items.push(item),
            None => groups.push(dto::TemplateGroupRes {
                category,
                items: vec![item],
            }),
        }
    }

    groups.sort_by(|left, right| {
        template_category_rank(&left.category)
            .cmp(&template_category_rank(&right.category))
            .then_with(|| left.category.cmp(&right.category))
    });
    let total = groups.iter().map(|group| group.items.len() as u64).sum();
    Ok(Res::ok(dto::TemplatesRes { groups, total }))
}

/// 另存为（`POST /business/flow/def/{id}/duplicate`）。
///
/// 「标准流程 → 我们自己的流程」的唯一入口：**复制定义**（节点 + 连线 + 审批人 + 元信息），
/// 生成一个新编码的 `DRAFT`（`is_system = false`），原标准流程一行不动。用户改完自己发布。
/// 逻辑不动（P2-6b 只收口语义与注释）：副本 `is_system = false` ⇒ 不再出现在默认口径的
/// 标准流程列表里，要看到它得带 `?scope=all`（或 legacy `?include_mine=1`）。
///
/// # 三条刻意的口径
///
/// 1. **只复制定义，不复制实例**：`biz_flow_instance` 一条也不碰 —— 副本拿去跑新单子，
///    历史审批记录仍挂在原模板的 def_id 上（审计口径不串）。
/// 2. **副本 `is_default = false`**：`uk_biz_flow_def_default` 允许每个 biz_type 只有一个默认流程，
///    副本若抢占默认，等于**悄悄改了客户在跑的规则**。要换成副本，由用户在流程定义页显式切换。
/// 3. **`is_system = false`**：副本是客户的，前端不标「标准」。
///
/// # 异常情况
///
/// - 400 + `65005`：新编码 / 新名称非法（格式 / 长度）；
/// - 404 + `65001`：源流程不存在 / 已软删；
/// - 409 + `10003`：新编码已被占用；
/// - 409 + `65006`：源流程一个版本都没有（没有可复制的图）。
#[nik::permission(
    path = "/business/flow/def/{id}/duplicate",
    method = "POST",
    permission = "business:flow:create"
)]
#[nik::post("/business/flow/def/{id}/duplicate")]
pub async fn duplicate(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::DuplicateReq>,
) -> ResResult<dto::DuplicateRes> {
    let operator = me_of(&claims)?;
    let def_id = parse_uuid_field(&id, "流程 id")?;
    let source = load_def(def_id).await?;
    let new_code = dto::normalize_flow_code(&req.new_flow_code).map_err(field_invalid)?;
    let new_name = dto::normalize_name(&req.new_flow_name).map_err(field_invalid)?;

    if model::flow_code_taken(&new_code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验流程编码"))?
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("流程编码 {new_code} 已被占用"),
        ));
    }

    // 复制源：当前发布版本 > 最新版本（与「另存为新草稿」同一口径）
    let source_version = match source.current_version_id {
        Some(version_id) => load_version(version_id).await?,
        None => model::versions_of(def_id)
            .await
            .map_err(|error| utils::db_error(error, "查询流程版本"))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ApiError::new(
                    409,
                    error_code::FLOW_VERSION_NOT_EDITABLE,
                    "该模板还没有任何版本，无法另存为",
                )
            })?,
    };
    let source_version_id = source_version.id;
    let source_version_no = source_version.version_no;

    let new_def = model::NewDef {
        id: Uuid::now_v7(),
        flow_code: new_code.clone(),
        flow_name: new_name.clone(),
        biz_type: source.biz_type.clone(),
        category: source.category.clone(),
        description: source.description.clone(),
        is_default: false,
        allow_auto_approve: source.allow_auto_approve,
    };
    let new_def_id = new_def.id;
    let note = format!("从模板 {} v{source_version_no} 另存为", source.flow_code);
    let note_for_tx = note.clone();
    let (version_id, node_count, edge_count) = db()
        .transaction(|tx| async move {
            model::create_def(&tx, &new_def, operator).await?;
            let version_id = model::create_version(&tx, new_def_id, 1, Some(&note_for_tx), operator).await?;
            let (node_count, edge_count) = model::copy_version_to_draft(
                &tx,
                new_def_id,
                source_version_id,
                version_id,
                Some(&note_for_tx),
                operator,
            )
            .await?;
            model::touch_version(&tx, version_id, Some(&note_for_tx), node_count, edge_count, operator).await?;
            Ok((version_id, node_count, edge_count))
        })
        .await
        .map_err(|error| utils::db_error(error, "另存为流程模板"))?;

    tracing::info!(
        source_def = %def_id,
        new_def = %new_def_id,
        %new_code,
        %version_id,
        node_count,
        edge_count,
        operator = %operator,
        "流程模板已另存为"
    );
    Ok(Res::ok(dto::DuplicateRes {
        id: new_def_id.to_string(),
        flow_code: new_code,
        flow_name: new_name,
        status: "DRAFT".to_string(),
        is_system: false,
        version_id: version_id.to_string(),
        version_no: 1,
        node_count,
        edge_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_row(status: &str) -> model::TaskRow {
        let now = chrono::Utc::now();
        model::TaskRow {
            id: Uuid::nil(),
            instance_id: Uuid::nil(),
            instance_node_id: Uuid::nil(),
            node_key: "n_approve".to_string(),
            round: 1,
            task_kind: "APPROVAL".to_string(),
            assignee_id: Uuid::nil(),
            assignee_name: Some("张三".to_string()),
            assignee_source: "USER".to_string(),
            assignee_source_ref: None,
            status: status.to_string(),
            is_added_sign: false,
            is_self_approval: false,
            is_auto: false,
            comment: None,
            attachments: serde_json::Value::Array(Vec::new()),
            transferred_from: None,
            transferred_to: None,
            due_at: None,
            remind_count: 0,
            created_at: now,
            completed_at: if status == "PENDING" { None } else { Some(now) },
            escalated_from_role: None,
            escalate_reason: None,
            assigned_by_rule: None,
            escalated_to_role: None,
        }
    }

    fn cc_row() -> model::CcRow {
        model::CcRow {
            id: Uuid::nil(),
            instance_id: Uuid::nil(),
            node_key: "n_approve".to_string(),
            round: 2,
            receiver_id: Uuid::nil(),
            receiver_name: Some("李四".to_string()),
            source_type: "USER".to_string(),
            source_ref: None,
            is_read: false,
            read_at: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn task_res_carries_d5_display_fields() {
        // D5 缺口 2：待办项必须有 flow_name / initiator_name / current_node_name，
        // 否则前端卡片只能显示 node_key 与单据号。
        let row = model::MyTaskRow {
            task: task_row("PENDING"),
            instance_no: "FLW-202610-000001".to_string(),
            title: "报销申请".to_string(),
            biz_type: "PO_APPROVE".to_string(),
            biz_id: Some(Uuid::nil()),
            biz_no: Some("PO-20261031-000003".to_string()),
            flow_name: "财务审批".to_string(),
            initiator_name: Some("王五".to_string()),
            instance_status: "PENDING".to_string(),
            node_name: "财务会签".to_string(),
            form_data: serde_json::json!({
                "order_no": "PO-20261031-000003",
                "supplier_id": "01a18def-1a2b-7329-a9d3-c6afa9e04220",
                "supplier_name": "示例供应商（种子）",
                "amount": 226.0,
            }),
        };
        let res = my_task_res(&row);
        assert_eq!(res.flow_name, "财务审批");
        assert_eq!(res.initiator_name, "王五");
        assert_eq!(res.current_node_name, "财务会签");
        assert_eq!(res.instance_no, "FLW-202610-000001");
        assert_eq!(res.title, "报销申请");
        // HOME-5：业务类型 / 业务 id / 业务单号随待办行下发 —— 工作台卡片靠它把
        // 「单据类型 + 单号」显示出来、并把点击路由映射到业务详情页。
        assert_eq!(res.biz_type, "PO_APPROVE");
        assert_eq!(res.biz_id, Uuid::nil().to_string());
        assert_eq!(res.biz_no, "PO-20261031-000003");
        assert_eq!(res.status, "PENDING");
        // P11：实例状态随行下发，前端才能挡住「实例已结束但待办还 PENDING」的孤儿行。
        assert_eq!(res.instance_status, "PENDING");
        // P1-FIX：表单快照随列表一起下发（待办卡片摘要不再逐单查 `/instance/{id}`）。
        assert_eq!(res.form_data["supplier_name"], "示例供应商（种子）");
        assert_eq!(res.form_data["amount"], 226.0);
        // 到达时间 = 任务创建时间（待办按它倒序）。
        assert_eq!(res.arrived_at, res.created_at);
    }

    #[test]
    fn my_task_res_degrades_missing_initiator_name() {
        // 发起人已删 → initiator_name 为 NULL，必须降级空串（前端不判 null）。
        let row = model::MyTaskRow {
            task: task_row("APPROVED"),
            instance_no: "FLW-202610-000002".to_string(),
            title: "请假".to_string(),
            // 通用流程（无关联业务单据）：三个业务字段都降级空串，前端据此走通用跳转。
            biz_type: "OTHER".to_string(),
            biz_id: None,
            biz_no: None,
            flow_name: "假勤审批".to_string(),
            initiator_name: None,
            instance_status: "APPROVED".to_string(),
            node_name: "主管审批".to_string(),
            // 无表单的实例 → `{}`（不是 `null`）：前端按对象判空。
            form_data: serde_json::json!({}),
        };
        let res = my_task_res(&row);
        assert_eq!(res.initiator_name, "");
        assert_eq!(res.current_node_name, "主管审批");
        assert_eq!(res.instance_status, "APPROVED");
        assert_eq!(res.form_data, serde_json::json!({}));
        assert_eq!(res.biz_type, "OTHER");
        assert_eq!(res.biz_id, "", "无业务单据 → 空串（不是 null，前端不判 null）");
        assert_eq!(res.biz_no, "");
        assert!(res.completed_at.is_some(), "已办行必有操作时间");
    }

    #[test]
    fn parse_since_field_accepts_rfc3339_and_rejects_garbage() {
        // 首页工作台「今日已办」：合法时间 → UTC；非法 → 400 + 65005，不静默当没传。
        let parsed = parse_since_field("2026-11-09T00:00:00+08:00", "since").expect("应能解析");
        assert_eq!(parsed.to_rfc3339(), "2026-11-08T16:00:00+00:00");
        let naive = parse_since_field("2026-11-09 00:00:00", "since").expect("应能解析无时区格式");
        assert_eq!(naive.to_rfc3339(), "2026-11-09T00:00:00+00:00");
        let err = parse_since_field("昨天", "since").expect_err("垃圾输入必须报错");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::FLOW_FIELD_INVALID);
    }

    #[test]
    fn cc_res_carries_d5_display_fields() {
        let row = model::MyCcRow {
            cc: cc_row(),
            instance_no: "FLW-202610-000003".to_string(),
            title: "采购".to_string(),
            flow_name: "采购审批".to_string(),
            initiator_name: Some("赵六".to_string()),
            node_name: "".to_string(),
        };
        let res = my_cc_res(&row);
        assert_eq!(res.flow_name, "采购审批");
        assert_eq!(res.initiator_name, "赵六");
        assert_eq!(res.current_node_name, "", "节点行缺失时降级空串");
        assert_eq!(res.arrived_at, res.created_at);
        assert_eq!(res.round, 2);
    }

    #[test]
    fn instance_res_exposes_form_data() {
        // D5 缺口 1：实例详情必须能读回发起时填的表单内容。
        let now = chrono::Utc::now();
        let row = model::InstanceRow {
            id: Uuid::nil(),
            instance_no: "FLW-202610-000004".to_string(),
            def_id: Uuid::nil(),
            version_id: Uuid::nil(),
            version_no: 1,
            flow_code: "finance".to_string(),
            flow_name: "财务审批".to_string(),
            biz_type: "FINANCE".to_string(),
            biz_id: None,
            biz_no: None,
            title: "报销 100 元".to_string(),
            status: "PENDING".to_string(),
            initiator_id: Uuid::nil(),
            initiator_name: Some("王五".to_string()),
            initiator_dept_id: None,
            form_data: serde_json::json!({ "amount": 100, "reason": "差旅" }),
            result_comment: None,
            auto_passed_nodes: 0,
            round_total: 1,
            prev_instance_id: None,
            revision: 1,
            started_at: now,
            ended_at: None,
            due_at: None,
            created_at: now,
            updated_at: now,
        };
        let res = instance_res(&row, None);
        assert_eq!(res.form_data["amount"], 100);
        assert_eq!(res.form_data["reason"], "差旅");
        assert_eq!(res.my_task_id, "");
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    fn row(code: &str, total: i64, approved: i64, rejected: i64, running: i64, sum: i64) -> model::EfficiencyRow {
        model::EfficiencyRow {
            flow_code: code.to_string(),
            flow_name: format!("{code}-名称"),
            total,
            approved,
            rejected,
            running,
            cancelled: 0,
            withdrawn: 0,
            finished: total - running,
            sum_duration_seconds: sum,
            max_duration_seconds: sum,
        }
    }

    /// 造 5 实例（3 通过 1 驳回 1 在跑）→ 总览数字与平均时长正确。
    #[test]
    fn overview_counts_five_instances() {
        let rows = vec![row("leave", 5, 3, 1, 1, 3600)];
        let overview = model::fold_overview(&rows);
        assert_eq!(overview.total, 5);
        assert_eq!(overview.approved, 3);
        assert_eq!(overview.rejected, 1);
        assert_eq!(overview.running, 1);
        assert_eq!(overview.finished, 4);
        assert_eq!(overview.avg_duration_seconds(), 900, "3600/4");
        assert_eq!(overview.approved_rate(), 0.6);
    }

    /// 无数据 → 全部 0（红线：不许 null），JSON 里也必须出现 0。
    #[test]
    fn empty_stats_are_zero_not_null() {
        let overview = model::fold_overview(&[]);
        assert_eq!(overview.total, 0);
        assert_eq!(overview.avg_duration_seconds(), 0);
        assert_eq!(overview.approved_rate(), 0.0);
        let res = StatsOverviewRes {
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            total: overview.total,
            approved: overview.approved,
            rejected: overview.rejected,
            pending: overview.running,
            cancelled: overview.cancelled,
            withdrawn: overview.withdrawn,
            finished: overview.finished,
            approved_rate: overview.approved_rate(),
            avg_duration_seconds: overview.avg_duration_seconds(),
        };
        let json = serde_json::to_string(&res).expect("序列化");
        assert!(!json.contains("null"), "不许下发 null：{json}");
        assert!(json.contains("\"avg_duration_seconds\":0"), "{json}");
        let empty = StatsBottleneckRes {
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            slowest_node_key: String::new(),
            slowest_node_name: String::new(),
            slowest_avg_dwell_seconds: 0,
            list: Vec::new(),
        };
        let json = serde_json::to_string(&empty).expect("序列化");
        assert!(!json.contains("null"), "不许下发 null：{json}");
    }

    /// 平均时长按「已结束数」加权（在跑实例不参与）。
    #[test]
    fn avg_uses_finished_only() {
        let mut done = row("a", 3, 2, 1, 0, 900);
        done.finished = 3;
        let mut running = row("b", 1, 0, 0, 1, 0);
        running.finished = 0;
        let overview = model::fold_overview(&[done, running]);
        assert_eq!(overview.total, 4);
        assert_eq!(overview.avg_duration_seconds(), 300, "900/3，不是 900/4");
    }

    /// 卡点取平均停留最久，同平均看样本数。
    #[test]
    fn slowest_picks_max_avg() {
        let rows = vec![
            model::BottleneckRow {
                node_key: "n_fast".into(),
                node_name: "快".into(),
                samples: 9,
                avg_dwell_seconds: 60,
                max_dwell_seconds: 90,
            },
            model::BottleneckRow {
                node_key: "n_slow".into(),
                node_name: "慢".into(),
                samples: 2,
                avg_dwell_seconds: 600,
                max_dwell_seconds: 900,
            },
            model::BottleneckRow {
                node_key: "n_tie".into(),
                node_name: "并列".into(),
                samples: 5,
                avg_dwell_seconds: 600,
                max_dwell_seconds: 700,
            },
        ];
        let slowest = model::slowest_of(&rows).expect("不能为空");
        assert_eq!(slowest.node_key, "n_tie", "平均相同取样本多的");
        assert!(model::slowest_of(&[]).is_none());
    }

    /// 时间参数：`YYYY-MM-DD` 按 UTC 零点、RFC3339 原样。
    #[test]
    fn parse_time_accepts_date_and_rfc3339() {
        let date = parse_stat_time(Some("2026-10-01"), "start")
            .expect("日期")
            .expect("有值");
        assert_eq!(date.to_rfc3339(), "2026-10-01T00:00:00+00:00");
        let rfc = parse_stat_time(Some("2026-10-01T08:30:00+08:00"), "start")
            .expect("rfc3339")
            .expect("有值");
        assert_eq!(rfc.to_rfc3339(), "2026-10-01T00:30:00+00:00");
        assert!(parse_stat_time(None, "start").expect("缺省").is_none());
        assert!(parse_stat_time(Some("  "), "start").expect("空白").is_none());
    }

    /// 非法时间 → 400 + 65005（不是 500，也不静默忽略）。
    #[test]
    fn parse_time_rejects_garbage() {
        let error = parse_stat_time(Some("2026/10/01"), "start").expect_err("必须报错");
        assert_eq!(error.code, error_code::FLOW_FIELD_INVALID);
        assert_eq!(error.status, 400);
    }

    /// 时间窗：缺省近 30 天；`start >= end` 报错；跨度超限报错。
    #[test]
    fn window_defaults_and_validates() {
        let now = chrono::Utc::now();
        let window = stats_window(&StatsQuery::default()).expect("缺省窗口");
        assert!(window.end >= now, "end 缺省=现在");
        assert_eq!(
            (window.end - window.start).num_days(),
            model::STATS_DEFAULT_DAYS,
            "缺省应回看 30 天"
        );
        assert!(window.start < window.end);
        let bad = stats_window(&StatsQuery {
            start: Some("2026-10-10".into()),
            end: Some("2026-10-01".into()),
        })
        .expect_err("start >= end 必须报错");
        assert_eq!(bad.code, error_code::FLOW_FIELD_INVALID);
        let widen = stats_window(&StatsQuery {
            start: Some("2020-01-01".into()),
            end: Some("2026-10-01".into()),
        })
        .expect_err("跨度超限必须报错");
        assert_eq!(widen.code, error_code::FLOW_FIELD_INVALID);
    }
}

#[cfg(test)]
mod template_scope_tests {
    use super::*;

    /// 缺省（两个参数都不传）= 只要标准流程。
    #[test]
    fn default_scope_is_standard() {
        assert_eq!(
            parse_template_scope(None, None).expect("缺省合法"),
            model::TemplateScope::Standard
        );
        assert_eq!(
            parse_template_scope(Some("  "), None).expect("空串按缺省"),
            model::TemplateScope::Standard
        );
    }

    /// `scope` 显式取值（大小写不敏感、去空白）。
    #[test]
    fn explicit_scope_wins() {
        assert_eq!(
            parse_template_scope(Some("all"), None).expect("all"),
            model::TemplateScope::All
        );
        assert_eq!(
            parse_template_scope(Some("  ALL "), None).expect("大小写 + 空白"),
            model::TemplateScope::All
        );
        assert_eq!(
            parse_template_scope(Some("Standard"), None).expect("standard"),
            model::TemplateScope::Standard
        );
    }

    /// legacy `include_mine=1` ≡ `scope=all`；且 `scope` 优先（两个都传时 scope 说了算）。
    #[test]
    fn legacy_include_mine_equals_scope_all() {
        assert_eq!(
            parse_template_scope(None, Some("1")).expect("include_mine=1"),
            model::TemplateScope::All
        );
        assert_eq!(
            parse_template_scope(None, Some("TRUE")).expect("大小写不敏感"),
            model::TemplateScope::All
        );
        assert_eq!(
            parse_template_scope(None, Some("0")).expect("0 视为假"),
            model::TemplateScope::Standard
        );
        // scope 优先：老参数不能把显式 standard 顶掉
        assert_eq!(
            parse_template_scope(Some("standard"), Some("1")).expect("scope 优先"),
            model::TemplateScope::Standard
        );
        assert_eq!(
            parse_template_scope(Some("all"), Some("0")).expect("scope 优先"),
            model::TemplateScope::All
        );
    }

    /// 非法 `scope` → 400 + `65005`（不静默退化成默认值，免得前端以为筛选生效了）。
    #[test]
    fn invalid_scope_is_rejected() {
        let error = parse_template_scope(Some("mine"), None).expect_err("非法值必须报错");
        assert_eq!(error.code, error_code::FLOW_FIELD_INVALID);
        assert!(
            error.msg.contains("scope"),
            "报错文案必须点出是 scope 参数（实际：{}）",
            error.msg
        );
    }
}
