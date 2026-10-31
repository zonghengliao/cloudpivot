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

use crate::business::flow::graph::{EdgeSpec, FlowGraph, Issue, NodeSpec, NodeType};
use crate::business::flow::task_bridge;
use crate::business::flow::{dto, model};
use crate::shared::error_code;
use crate::utils;
use serde_json::Map;

// ==================== 小工具 ====================

/// 解析路径里的 UUID（非法 → `400 + 65005`，不静默当 404）。
fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ApiError::new(
            400,
            error_code::FLOW_FIELD_INVALID,
            format!("{label}格式非法"),
        )
    })
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
        join_mode: node.join_mode.as_str().to_string(),
        fork_mode: node.fork_mode.as_str().to_string(),
        timeout_hours: node.timeout_hours,
        timeout_action: node.timeout_action.clone(),
        remind_before_hours: node.remind_before_hours,
        max_remind_times: node.max_remind_times,
        cc_on_approved: node.cc_on_approved,
        config: node.config.clone(),
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
#[nik::permission(
    path = "/business/flow/def/page",
    method = "GET",
    permission = "business:flow:read"
)]
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
#[nik::permission(
    path = "/business/flow/def/{id}",
    method = "GET",
    permission = "business:flow:read"
)]
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
#[nik::permission(
    path = "/business/flow/def",
    method = "POST",
    permission = "business:flow:create"
)]
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
    let description =
        dto::normalize_description(req.description.as_deref()).map_err(field_invalid)?;

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
            let version_id =
                model::create_version(&tx, def_id, 1, Some("初始版本"), operator).await?;
            model::replace_graph(&tx, version_id, &graph).await?;
            model::touch_version(
                &tx,
                version_id,
                Some("初始版本"),
                node_count,
                edge_count,
                operator,
            )
            .await?;
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
        patch.description =
            Some(dto::normalize_description(description.as_deref()).map_err(field_invalid)?);
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

    let issues = graph.validate();
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
    let change_note = req.normalized_change_note();
    let node_count = graph.nodes.len() as i64;
    let edge_count = graph.edges.len() as i64;

    // 保存期只提示、不阻断（架构 §10.4.3）
    let warnings: Vec<String> = graph
        .validate()
        .iter()
        .map(|issue| format!("[{}] {}", issue.rule, issue.message))
        .collect();

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
    let issues = graph.validate();
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
        .map(|text| {
            text.chars()
                .take(dto::MAX_CHANGE_NOTE_CHARS)
                .collect::<String>()
        });

    let source_no = source.version_no;
    let source_id = source.id;
    let note_for_write = change_note.clone();
    let (new_version_id, node_count, edge_count) = db()
        .transaction(|tx| async move {
            let next_no = model::max_version_no(&tx, def_id).await? + 1;
            let new_version_id =
                model::create_version(&tx, def_id, next_no, note_for_write.as_deref(), operator)
                    .await?;
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
// | 15 | POST | `/business/flow/instance/{id}/withdraw` | `business:flow:instance:withdraw` |
// | 16 | POST | `/business/flow/task/{id}/approve` | `business:flow:approve` |
// | 17 | POST | `/business/flow/task/{id}/reject` | `business:flow:approve` |
// | 18 | POST | `/business/flow/task/{id}/transfer` | `business:flow:approve` |
// | 19 | POST | `/business/flow/task/{id}/add-sign` | `business:flow:approve` |
// | 20 | POST | `/business/flow/task/batch` | `business:flow:approve` |
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
        initiator_dept_id: row
            .initiator_dept_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
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
    /// 流程名称。
    flow_name: &'a str,
    /// 发起人姓名。
    initiator_name: &'a str,
    /// 当前节点中文名。
    current_node_name: &'a str,
    /// 所属审批实例的状态（P11：前端防御要用，见 `dto::TaskRes::instance_status`）。
    instance_status: &'a str,
}

fn task_res(row: &model::TaskRow, ctx: &TaskCtx<'_>) -> dto::TaskRes {
    dto::TaskRes {
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
        task_kind: row.task_kind.clone(),
        assignee_id: row.assignee_id.to_string(),
        assignee_name: row.assignee_name.clone().unwrap_or_default(),
        assignee_source: row.assignee_source.clone(),
        status: row.status.clone(),
        instance_status: ctx.instance_status.to_string(),
        is_added_sign: row.is_added_sign,
        is_self_approval: row.is_self_approval,
        is_auto: row.is_auto,
        comment: row.comment.clone().unwrap_or_default(),
        transferred_from: row
            .transferred_from
            .map(|id| id.to_string())
            .unwrap_or_default(),
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

/// `uk_biz_flow_instance_biz_active` 冲突 = 同一业务单据已有在跑实例（并发双击的库级兜底）。
fn is_biz_active_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_flow_instance_biz_active"))
            .unwrap_or(false),
        other => other
            .to_string()
            .contains("uk_biz_flow_instance_biz_active"),
    }
}

/// 事务跑完把锁内收集的副作用发出去（**锁外发送**，§4.5.1 第 ④ 条）。
async fn drain(out: &mut engine::ActionOut) -> Option<ApiError> {
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    out.rejected.take()
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
    } else if let Some(code) = req
        .flow_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty())
    {
        match model::find_def_by_code(code)
            .await
            .map_err(|error| utils::db_error(error, "按编码查询流程定义"))?
        {
            Some(row) => row,
            None => return Err(model::not_found()),
        }
    } else if let Some(biz_type) = req
        .biz_type
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
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
        return Err(field_invalid(
            "必须指定流程：def_id / flow_code / biz_type 三选一",
        ));
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
    let biz_id = match req
        .biz_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
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
        .transaction(|tx| async move { engine::start(&tx, &start).await })
        .await;
    let mut out = match result {
        Ok(out) => out,
        Err(error) if is_biz_active_duplicate(&error) => {
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
    let allowed = row.initiator_id == me
        || mine.contains(&row.id)
        || claims.has_permission("business:flow:admin");
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
    let ctx_of = |node_key: &str| TaskCtx {
        instance_no: row.instance_no.as_str(),
        title: row.title.as_str(),
        flow_name: row.flow_name.as_str(),
        initiator_name: initiator_name.as_str(),
        current_node_name: node_names.get(node_key).copied().unwrap_or(""),
        instance_status: row.status.as_str(),
    };

    Ok(Res::ok(dto::InstanceDetailRes {
        instance: instance_res(&row, my_task.as_ref().map(|task| task.id)),
        nodes: nodes.iter().map(node_progress_res).collect(),
        timeline: timeline
            .iter()
            .map(|task| task_res(task, &ctx_of(&task.node_key)))
            .collect(),
        cc: cc
            .iter()
            .map(|entry| cc_res(entry, &ctx_of(&entry.node_key)))
            .collect(),
        my_task: my_task
            .as_ref()
            .map(|task| task_res(task, &ctx_of(&task.node_key))),
        auto_passed_nodes: row.auto_passed_nodes,
    }))
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
    // P16：从审批中心撤回的也可能是一张「收货单作废」单 → 单据必须回到已过账。
    // 不做这一步就是 P11 的老坑：审批单 WITHDRAWN 了，单据永远停在「作废审批中」。
    crate::business::erp::stockin_bridge::on_cancel_settled(Some(instance_id), "WITHDRAWN").await;
    // P17：也可能是「应付付款」单 → 付款记录标 REJECTED（账一分没动，可重新提交）
    crate::business::erp::payable_bridge::on_payment_settled(Some(instance_id), "WITHDRAWN").await;
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
    let mut out = db()
        .transaction(|tx| async move {
            engine::approve(&tx, task_id, operator, comment.as_deref()).await
        })
        .await
        .map_err(|error| utils::db_error(error, "审批同意"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // D3b：审批单可能已落终态（APPROVED）→ 以 FlowCallback 身份推进任务
    // （**事务已提交**：回调失败只记日志，不回滚审批单）
    task_bridge::on_instance_settled(out.instance_id, &out.instance_status).await;
    // W3b：同一条审批单也可能是「库存调拨单」发起的 —— 与任务回调并列、互不影响
    task_bridge::on_transfer_settled(out.instance_id, &out.instance_status).await;
    // P16：也可能是「收货单作废」发起的 —— 通过则执行六本账回滚，驳回则打回已过账
    crate::business::erp::stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status)
        .await;
    // P17：也可能是「应付付款」发起的 —— 通过才记账（应付回写 + 借 2202 / 贷 1002 凭证）
    crate::business::erp::payable_bridge::on_payment_settled(out.instance_id, &out.instance_status)
        .await;
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
        .transaction(|tx| async move {
            engine::reject(&tx, task_id, operator, &comment, return_to).await
        })
        .await
        .map_err(|error| utils::db_error(error, "审批驳回"))?;
    if let Some(error) = drain(&mut out).await {
        return Err(error);
    }
    // D3b：驳回并终止（REJECTED）→ 以 FlowCallback 身份把任务打回 DOING
    // （「退回上一节点」仍是 PENDING，回调不推进；落到 REJECTED 才动任务）
    task_bridge::on_instance_settled(out.instance_id, &out.instance_status).await;
    // W3b：同一条审批单也可能是「库存调拨单」发起的 —— 与任务回调并列、互不影响
    task_bridge::on_transfer_settled(out.instance_id, &out.instance_status).await;
    // P16：收货单作废被驳回 → 单据回到已过账（账一动没动）
    crate::business::erp::stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status)
        .await;
    // P17：付款审批被驳回 / 驳回终止 → 付款记录标 REJECTED（账一分没动，可重新提交）
    crate::business::erp::payable_bridge::on_payment_settled(out.instance_id, &out.instance_status)
        .await;
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
        .transaction(|tx| async move {
            engine::transfer(&tx, task_id, operator, to_user_id, reason.as_deref()).await
        })
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
        return Err(ApiError::new(
            400,
            error_code::FLOW_ADD_SIGN_INVALID,
            "加签人不能为空",
        ));
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
/// # 异常情况
///
/// - 400 + `65005`：`task_ids` 为空 / 超过 50 条 / 动作非法 / 驳回意见不足 5 字。
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
    let operator = me_of(&claims)?;
    if req.task_ids.is_empty() {
        return Err(field_invalid("task_ids 不能为空"));
    }
    if req.task_ids.len() > dto::MAX_BATCH_TASKS {
        return Err(field_invalid(format!(
            "单次批量审批最多 {} 条",
            dto::MAX_BATCH_TASKS
        )));
    }
    let approve = match req.action.trim().to_ascii_uppercase().as_str() {
        "APPROVE" => true,
        "REJECT" => false,
        other => {
            return Err(field_invalid(format!(
                "动作 {other} 非法（只允许 APPROVE / REJECT）"
            )));
        }
    };
    let comment = if approve {
        dto::normalize_comment(req.comment.as_deref()).map_err(field_invalid)?
    } else {
        Some(
            dto::normalize_reject_comment(req.comment.as_deref().unwrap_or(""))
                .map_err(field_invalid)?,
        )
    };
    let return_to = engine::ReturnTo::parse(req.return_to.as_deref().unwrap_or(""));
    if !approve && return_to.needs_admin() && !claims.has_permission("business:flow:admin") {
        return Err(ApiError::new(
            403,
            error_code::FLOW_FORBIDDEN,
            "批量驳回并终止 / 退任意节点需要流程管理员权限",
        ));
    }

    let mut items = Vec::with_capacity(req.task_ids.len());
    let mut succeeded = 0usize;
    for raw in &req.task_ids {
        let task_id = match Uuid::parse_str(raw.trim()) {
            Ok(id) => id,
            Err(_) => {
                items.push(dto::BatchItemRes {
                    task_id: raw.clone(),
                    ok: false,
                    code: error_code::FLOW_FIELD_INVALID,
                    message: "待办 id 格式非法".to_string(),
                });
                continue;
            }
        };
        let comment = comment.clone();
        let return_to = return_to.clone();
        // 逐条独立事务：一条 409 不影响另外 49 条
        let result = db()
            .transaction(|tx| async move {
                if approve {
                    engine::approve(&tx, task_id, operator, comment.as_deref()).await
                } else {
                    engine::reject(
                        &tx,
                        task_id,
                        operator,
                        comment.as_deref().unwrap_or("批量驳回"),
                        return_to,
                    )
                    .await
                }
            })
            .await;
        match result {
            Ok(mut out) => {
                let rejected = drain(&mut out).await;
                match rejected {
                    None => {
                        succeeded += 1;
                        items.push(dto::BatchItemRes {
                            task_id: raw.clone(),
                            ok: true,
                            code: 0,
                            message: String::new(),
                        });
                    }
                    Some(error) => items.push(dto::BatchItemRes {
                        task_id: raw.clone(),
                        ok: false,
                        code: error.code,
                        message: error.msg,
                    }),
                }
            }
            Err(error) => {
                let api = utils::db_error(error, "批量审批");
                items.push(dto::BatchItemRes {
                    task_id: raw.clone(),
                    ok: false,
                    code: api.code,
                    message: api.msg,
                });
            }
        }
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
    task_res(
        &row.task,
        &TaskCtx {
            instance_no: &row.instance_no,
            title: &row.title,
            flow_name: &row.flow_name,
            initiator_name: &initiator_name,
            current_node_name: &row.node_name,
            instance_status: &row.instance_status,
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
            flow_name: &row.flow_name,
            initiator_name: &initiator_name,
            current_node_name: &row.node_name,
            // 抄送出参（`CcRes`）没有实例状态这一列，也不需要（抄送不是待办，点不动的风险只存在于
            // 「待我审批」）；`cc_res` 不读这个字段，这里给空串而不是去猜。
            instance_status: "",
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
    let result = model::page_my_tasks(
        me,
        model::MY_TODO_STATUSES,
        &dto::TASK_KINDS,
        model::MyTaskOrder::Arrived,
        page_no,
        page_size,
        // P11：待办只认实例还在跑的单子（历史脏数据不进审批人的列表）
        true,
    )
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
    let result = model::page_my_tasks(
        me,
        model::MY_DONE_STATUSES,
        &dto::TASK_KINDS,
        model::MyTaskOrder::Operated,
        page_no,
        page_size,
        // 已办是历史记录：实例结束与否都要显示（不受 P11 的「只看在跑」约束）
        false,
    )
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
            flow_name: "财务审批".to_string(),
            initiator_name: Some("王五".to_string()),
            instance_status: "PENDING".to_string(),
            node_name: "财务会签".to_string(),
        };
        let res = my_task_res(&row);
        assert_eq!(res.flow_name, "财务审批");
        assert_eq!(res.initiator_name, "王五");
        assert_eq!(res.current_node_name, "财务会签");
        assert_eq!(res.instance_no, "FLW-202610-000001");
        assert_eq!(res.title, "报销申请");
        assert_eq!(res.status, "PENDING");
        // P11：实例状态随行下发，前端才能挡住「实例已结束但待办还 PENDING」的孤儿行。
        assert_eq!(res.instance_status, "PENDING");
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
            flow_name: "假勤审批".to_string(),
            initiator_name: None,
            instance_status: "APPROVED".to_string(),
            node_name: "主管审批".to_string(),
        };
        let res = my_task_res(&row);
        assert_eq!(res.initiator_name, "");
        assert_eq!(res.current_node_name, "主管审批");
        assert_eq!(res.instance_status, "APPROVED");
        assert!(res.completed_at.is_some(), "已办行必有操作时间");
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

    fn row(
        code: &str,
        total: i64,
        approved: i64,
        rejected: i64,
        running: i64,
        sum: i64,
    ) -> model::EfficiencyRow {
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
        assert!(
            parse_stat_time(Some("  "), "start")
                .expect("空白")
                .is_none()
        );
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
