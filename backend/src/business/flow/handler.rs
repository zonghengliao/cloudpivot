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

use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;
use axum::extract::{Path, Query};

use crate::business::flow::graph::{EdgeSpec, FlowGraph, Issue, NodeSpec, NodeType};
use crate::business::flow::{dto, model};
use crate::shared::error_code;
use crate::utils;

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
#[alun::permission(
    path = "/business/flow/def/page",
    method = "GET",
    permission = "business:flow:read"
)]
#[alun::get("/business/flow/def/page")]
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
#[alun::permission(
    path = "/business/flow/def/{id}",
    method = "GET",
    permission = "business:flow:read"
)]
#[alun::get("/business/flow/def/{id}")]
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
#[alun::permission(
    path = "/business/flow/def",
    method = "POST",
    permission = "business:flow:create"
)]
#[alun::post("/business/flow/def")]
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
#[alun::permission(
    path = "/business/flow/def/{id}",
    method = "PUT",
    permission = "business:flow:update"
)]
#[alun::put("/business/flow/def/{id}")]
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
#[alun::permission(
    path = "/business/flow/def/{id}",
    method = "DELETE",
    permission = "business:flow:delete"
)]
#[alun::delete("/business/flow/def/{id}")]
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
#[alun::permission(
    path = "/business/flow/def/{id}/publish",
    method = "POST",
    permission = "business:flow:publish"
)]
#[alun::post("/business/flow/def/{id}/publish")]
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
#[alun::permission(
    path = "/business/flow/def/{id}/offline",
    method = "POST",
    permission = "business:flow:publish"
)]
#[alun::post("/business/flow/def/{id}/offline")]
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
#[alun::permission(
    path = "/business/flow/def/{id}/versions",
    method = "GET",
    permission = "business:flow:read"
)]
#[alun::get("/business/flow/def/{id}/versions")]
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
#[alun::permission(
    path = "/business/flow/version/{vid}",
    method = "GET",
    permission = "business:flow:read"
)]
#[alun::get("/business/flow/version/{vid}")]
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
#[alun::permission(
    path = "/business/flow/version/{vid}/graph",
    method = "PUT",
    permission = "business:flow:update"
)]
#[alun::put("/business/flow/version/{vid}/graph")]
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
#[alun::permission(
    path = "/business/flow/version/{vid}/validate",
    method = "POST",
    permission = "business:flow:update"
)]
#[alun::post("/business/flow/version/{vid}/validate")]
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
#[alun::permission(
    path = "/business/flow/def/{id}/draft",
    method = "POST",
    permission = "business:flow:update"
)]
#[alun::post("/business/flow/def/{id}/draft")]
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
