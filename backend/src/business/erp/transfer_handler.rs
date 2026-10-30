//! 调拨单接口层（批次 D · W3b）：7 个接口 + 6 态状态机 + 审批接入。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/transfer/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/transfer/page` | `business:erp:transfer:list` | 分页（状态 / 两仓 / 单号） |
//! | GET | `/business/erp/transfer/{id}` | `business:erp:transfer:detail` | 详情（含明细 + 每行序列号 id） |
//! | POST | `/business/erp/transfer` | `business:erp:transfer:create` | 建单（`DRAFT` + `TK-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/transfer/{id}` | `business:erp:transfer:update` | 改单（**仅 `DRAFT`**） |
//! | PATCH | `/business/erp/transfer/{id}/status` | `business:erp:transfer:status` | 状态流转（6 态） |
//! | DELETE | `/business/erp/transfer/{id}` | `business:erp:transfer:delete` | 软删（仅 `DRAFT` / `CANCELLED`） |
//! | GET | `/business/erp/transfer/{id}/serials` | `business:erp:transfer:detail` | 涉及序列号（按明细分组） |
//!
//! 路由顺序说明：`/transfer/page` 是**静态段**、`/transfer/{id}` 是动态段，axum 的 matchit
//! 静态优先，`GET /transfer/page` 不会被 `{id}` 吃掉（W2b 的 `/serial/page` 同款实测）。
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库」
//! 收进 `create_transfer` / `update_transfer` / `change_transfer_status` / `delete_transfer`
//! 之后，单测走的是**同一条代码路径**（只跳过认证），比另写一份测试逻辑更能钉住行为。
//!
//! # 审批接入（卡面 §三）
//!
//! ```text
//! DRAFT → PENDING：查已发布流程（biz_type='STOCK_TRANSFER' 或 flow_code='stock_transfer'）
//!   · 有  → 同一事务里 engine::start 发起审批单 + 回写 approval_instance_id
//!   · 没有 → 只把状态推到 PENDING（降级：PENDING → APPROVED 允许人工推）
//! 审批结论回写（flow::task_bridge::on_transfer_settled，见该函数）：
//!   · APPROVED → 调拨单 APPROVED
//!   · REJECTED → 调拨单打回 DRAFT（审批意见写 reason，时间戳清空以满足 ck_biz_transfer_state）
//! ```
//!
//! **发起审批与状态写入同事务**：审批单建不出来就整笔回滚（否则会出现「调拨单已待审批、
//! 审批单没建、谁也不知道该等谁」这种比不接审批更坏的状态）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 调拨单是仓储作业单据（V0140 三张表都没有 dept_id / 归属人列），
//! 可见性由 business:erp:transfer:* 权限码控制；列表分页走 model 的 scope::paginate_unscoped。
//! ```
//!
//! # 不碰库存（W3c 的活）
//!
//! 本卡只写单据头状态与明细行，**不动** `biz_inventory` / `biz_inventory_txn` /
//! `biz_material_serial.status`：`IN_TRANSIT → DONE` 的库存双写是 W3c，
//! 在这里顺手写会与 W3c 的派生口径（数量由序列号算）打架。

use axum::Json;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::transfer_dto::{
    CreateTransferReq, TransferDetailRes, TransferFilter, TransferItemRes, TransferRes,
    TransferSerialGroupRes, TransferSerialRes, TransferSerialsRes, TransferStatusReq,
};
use crate::business::erp::transfer_model::{
    self, ALL_STATUSES, NewItem, NewTransfer, STATUS_PENDING, TransferQuery, TransferRow,
    transition_effect, transition_rejected_message,
};
use crate::business::flow::{engine, model as flow_model};
use crate::shared::error_code;
use crate::utils;

// ==================== 共用辅助 ====================

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 可空 UUID：空串 / 缺席都是 `None`。
fn optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_uuid(text, field).map(Some),
        None => Ok(None),
    }
}

/// 操作人（`create_by` / `update_by`）。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 不存在统一 404。
fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 状态冲突统一 409（非法流转 / 非 DRAFT 改单 / 非 DRAFT|CANCELLED 删除）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
///
/// 判据按**变体**而不是文案：`DbError::UniqueViolation` 的 `Display` 里没有
/// `duplicate key` 字样（W1c 实测），按文案匹配会把 409 判成 500。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "调拨单号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（状态与时间戳必须匹配 ck_biz_transfer_state；\
                 调出/调入仓库不能相同）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::ForeignKeyViolation { constraint, .. } => ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "引用的数据不存在（外键 {}）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 引用校验（一次查完，落库前） ====================

/// DTO 明细 → 数据层明细（解析物料 / 库位 / 序列号引用并校验归属）。
///
/// 校验顺序：**物料 → 调出库位 → 调入库位 → 序列号**，任何一条不过都 `400`，
/// 一行都不落库（调用方在事务外先跑这个）。
async fn prepare_items(
    req: &CreateTransferReq,
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
) -> std::result::Result<Vec<NewItem>, ApiError> {
    let mut items = Vec::with_capacity(req.items.len());
    let mut payload = String::new();
    let mut expected = 0_usize;
    for (index, item) in req.items.iter().enumerate() {
        let material_id = parse_uuid(&item.material_id, &format!("items[{index}].material_id"))?;
        transfer_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| reference_missing(format!("items[{index}].material_id 物料不存在")))?;

        let from_location_id = optional_uuid(
            item.from_location_id.as_deref(),
            &format!("items[{index}].from_location_id"),
        )?;
        if let Some(location_id) = from_location_id {
            let (_, warehouse_id) = transfer_model::inventory_location_ref(location_id)
                .await
                .map_err(|error| utils::db_error(error, "查询调出库位"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "items[{index}].from_location_id 库位库存行不存在\
                         （location_id 指向 biz_inventory_location.id）"
                    ))
                })?;
            if warehouse_id != from_warehouse_id {
                return Err(reference_missing(format!(
                    "items[{index}].from_location_id 不属于调出仓库"
                )));
            }
        }
        let to_location_id = optional_uuid(
            item.to_location_id.as_deref(),
            &format!("items[{index}].to_location_id"),
        )?;
        if let Some(location_id) = to_location_id {
            let (_, warehouse_id) = transfer_model::inventory_location_ref(location_id)
                .await
                .map_err(|error| utils::db_error(error, "查询调入库位"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "items[{index}].to_location_id 库位库存行不存在\
                         （location_id 指向 biz_inventory_location.id）"
                    ))
                })?;
            if warehouse_id != to_warehouse_id {
                return Err(reference_missing(format!(
                    "items[{index}].to_location_id 不属于调入仓库"
                )));
            }
        }

        let mut serial_ids = Vec::with_capacity(item.serial_ids.len());
        for (serial_index, raw) in item.serial_ids.iter().enumerate() {
            let serial_id = parse_uuid(raw, &format!("items[{index}].serial_ids[{serial_index}]"))?;
            serial_ids.push(serial_id);
            payload.push_str(&format!("{index}|{serial_id}|{material_id}\n"));
            expected += 1;
        }

        items.push(NewItem {
            material_id,
            from_location_id,
            to_location_id,
            qty: item.qty.round_dp(4),
            serial_ids,
        });
    }

    if expected > 0 {
        let payload = payload.trim_end().to_string();
        let valid = transfer_model::count_valid_serials(&payload)
            .await
            .map_err(|error| utils::db_error(error, "校验序列号"))?;
        if usize::try_from(valid).unwrap_or(0) != expected {
            return Err(reference_missing(format!(
                "提交了 {expected} 个序列号，其中只有 {valid} 个是「该物料 + 在库（IN_STOCK）+ 未删除」：\
                 序列号不存在 / 不属于该物料 / 已经出库的都不能随行调拨"
            )));
        }
    }
    Ok(items)
}

// ==================== ① 分页 ====================

/// 调拨单分页（状态 / 调出仓 / 调入仓 / 单号前缀）。
#[nik::permission(
    path = "/business/erp/transfer/page",
    method = "GET",
    permission = "business:erp:transfer:list"
)]
#[nik::get("/business/erp/transfer/page")]
pub async fn transfer_page(
    Query(filter): Query<TransferFilter>,
) -> ResResult<PageData<Vec<TransferRes>>> {
    let filter = filter.validated()?;
    let query = TransferQuery {
        status: filter.status.clone(),
        from_warehouse_id: optional_uuid(filter.from_warehouse_id.as_deref(), "from_warehouse_id")?,
        to_warehouse_id: optional_uuid(filter.to_warehouse_id.as_deref(), "to_warehouse_id")?,
        transfer_no: filter.transfer_no.clone(),
        page: filter.page,
        page_size: filter.page_size,
    };
    let (rows, total) = transfer_model::page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询调拨单"))?;
    let mut ids = Vec::with_capacity(rows.len() * 2);
    for row in &rows {
        ids.push(row.from_warehouse_id);
        ids.push(row.to_warehouse_id);
    }
    let names = transfer_model::warehouse_names(&ids)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库名称"))?;
    let list = rows
        .iter()
        .map(|row| {
            TransferRes::of(
                row,
                names.get(&row.from_warehouse_id).cloned(),
                names.get(&row.to_warehouse_id).cloned(),
            )
        })
        .collect();
    let (page, page_size) =
        crate::business::erp::model::normalize_page(filter.page, filter.page_size);
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

// ==================== ② 详情 ====================

/// 详情组装（handler 与建单 / 改单 / 流转的返回共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<TransferDetailRes, ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let items = transfer_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨明细"))?;
    let serials = transfer_model::serials_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨序列号"))?;
    let mut by_item: std::collections::BTreeMap<Uuid, Vec<String>> =
        std::collections::BTreeMap::new();
    for serial in &serials {
        by_item
            .entry(serial.item_id)
            .or_default()
            .push(serial.serial_id.to_string());
    }
    let names = transfer_model::warehouse_names(&[row.from_warehouse_id, row.to_warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询仓库名称"))?;
    let item_res = items
        .iter()
        .map(|item| {
            TransferItemRes::of(
                item,
                item.material_code.clone(),
                item.material_name.clone(),
                by_item.remove(&item.id).unwrap_or_default(),
            )
        })
        .collect();
    Ok(TransferDetailRes {
        transfer: TransferRes::of(
            &row,
            names.get(&row.from_warehouse_id).cloned(),
            names.get(&row.to_warehouse_id).cloned(),
        ),
        items: item_res,
    })
}

/// 调拨单详情（含明细 + 每行序列号 id）。
#[nik::permission(
    path = "/business/erp/transfer/{id}",
    method = "GET",
    permission = "business:erp:transfer:detail"
)]
#[nik::get("/business/erp/transfer/{id}")]
pub async fn transfer_detail(Path(id): Path<String>) -> ResResult<TransferDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 建单 ====================

/// 建单核心逻辑（handler 与单测共用）。
///
/// 校验顺序：DTO 校验（同仓 / 空明细 / 数量 / 序列号重复）→ 两仓存在 → 明细引用 →
/// 取号 + 落库（一个事务）。
pub async fn create_transfer(
    req: &CreateTransferReq,
    operator: Option<Uuid>,
    applicant_name: Option<&str>,
) -> std::result::Result<Uuid, ApiError> {
    req.validate()?;
    let from_warehouse_id = parse_uuid(&req.from_warehouse_id, "from_warehouse_id")?;
    let to_warehouse_id = parse_uuid(&req.to_warehouse_id, "to_warehouse_id")?;
    transfer_model::warehouse_name(from_warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调出仓库"))?
        .ok_or_else(|| reference_missing("from_warehouse_id 仓库不存在或已删除"))?;
    transfer_model::warehouse_name(to_warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调入仓库"))?
        .ok_or_else(|| reference_missing("to_warehouse_id 仓库不存在或已删除"))?;

    let items = prepare_items(req, from_warehouse_id, to_warehouse_id).await?;
    let total_qty = req.total_qty();
    let new = NewTransfer {
        from_warehouse_id,
        to_warehouse_id,
        transfer_date: req.transfer_date,
        reason: req
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        applicant_id: operator,
        applicant_name: applicant_name
            .map(str::trim)
            .filter(|text| !text.is_empty()),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match transfer_model::create(&tx, &new, &items, total_qty, operator, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建调拨单"))
        }
    }
}

/// 新建调拨单（`status = DRAFT`，单号 `TK-yyyyMMdd-000001`）。
#[nik::permission(
    path = "/business/erp/transfer",
    method = "POST",
    permission = "business:erp:transfer:create"
)]
#[nik::post("/business/erp/transfer")]
pub async fn transfer_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateTransferReq>,
) -> ResResult<TransferDetailRes> {
    let operator = operator_of(&claims);
    let id = create_transfer(&req, operator, claims.username.as_deref()).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 改单 ====================

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
pub async fn update_transfer(
    id: Uuid,
    req: &CreateTransferReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    req.validate()?;
    let current = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    if current.status != transfer_model::STATUS_DRAFT {
        return Err(conflict(format!(
            "调拨单当前是「{}」（{}），只有草稿（DRAFT）能编辑：\
             已提交审批的单据请先撤回或走审批驳回",
            transfer_model::status_label(&current.status),
            current.status
        )));
    }
    let from_warehouse_id = parse_uuid(&req.from_warehouse_id, "from_warehouse_id")?;
    let to_warehouse_id = parse_uuid(&req.to_warehouse_id, "to_warehouse_id")?;
    transfer_model::warehouse_name(from_warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调出仓库"))?
        .ok_or_else(|| reference_missing("from_warehouse_id 仓库不存在或已删除"))?;
    transfer_model::warehouse_name(to_warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调入仓库"))?
        .ok_or_else(|| reference_missing("to_warehouse_id 仓库不存在或已删除"))?;
    let items = prepare_items(req, from_warehouse_id, to_warehouse_id).await?;
    let total_qty = req.total_qty();
    let new = NewTransfer {
        from_warehouse_id,
        to_warehouse_id,
        transfer_date: req.transfer_date,
        reason: req
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        applicant_id: current.applicant_id,
        applicant_name: current.applicant_name.as_deref(),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    match transfer_model::update(&tx, id, &new, &items, total_qty, operator, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            // 读状态与写状态之间被别人推走（并发）：按冲突处理，不静默成功
            Err(conflict(
                "调拨单状态在本次编辑期间已被改动（只有草稿能改），请刷新后重试",
            ))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交改单事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "编辑调拨单"))
        }
    }
}

/// 编辑调拨单（仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/transfer/{id}",
    method = "PUT",
    permission = "business:erp:transfer:update"
)]
#[nik::put("/business/erp/transfer/{id}")]
pub async fn transfer_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreateTransferReq>,
) -> ResResult<TransferDetailRes> {
    let id = parse_uuid(&id, "id")?;
    update_transfer(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑤ 状态流转 ====================

/// 一次流转的全部上下文（事务内用；字段多，收成结构体而不是 8 个参数）。
struct TransitionContext<'a> {
    id: Uuid,
    from: &'a str,
    transfer_no: &'a str,
    effect: &'a transfer_model::TransitionEffect,
    cancel_reason: Option<&'a str>,
    transit_warehouse_id: Option<Uuid>,
    operator: Uuid,
    now: DateTime<Utc>,
    /// 提交审批时要发起的审批单参数（`None` = 没配流程 / 其它目标态）。
    start: Option<engine::StartReq>,
}

/// 事务内的状态写入（`affected = 0` 表示 CAS 失败，由调用方翻 409）。
async fn apply_transition(tx: &Transaction, ctx: &TransitionContext<'_>) -> DbResult<u64> {
    let mut approval_instance_id = None;
    if let Some(start) = ctx.start.as_ref() {
        let out = engine::start(tx, start).await?;
        approval_instance_id = Some(out.instance_id.unwrap_or(start.instance_id));
        tracing::info!(
            transfer_id = %ctx.id,
            transfer_no = %ctx.transfer_no,
            instance_id = ?approval_instance_id,
            instance_status = %out.instance_status,
            flow_code = %start.flow_code,
            "提交调拨单已自动发起审批单"
        );
    }
    let affected = transfer_model::apply_status(
        tx,
        ctx.id,
        ctx.from,
        ctx.effect,
        ctx.cancel_reason,
        ctx.transit_warehouse_id,
        Some(ctx.operator),
        approval_instance_id,
        Some(ctx.operator),
        ctx.now,
    )
    .await?;
    // 撤回：调拨单从 PENDING 变 CANCELLED 时把在跑的审批单一起终止（否则
    // uk_biz_flow_instance_biz_active 会让「取消后重新提交」永久 409）
    if affected > 0 && ctx.effect.cancelled && ctx.from == transfer_model::STATUS_PENDING {
        let cancelled = flow_model::cancel_pending_of_biz(
            tx,
            ctx.id,
            ctx.transfer_no,
            "调拨单已撤回，审批单自动终止",
        )
        .await?;
        if cancelled.instances > 0 {
            tracing::info!(
                transfer_id = %ctx.id,
                instances = cancelled.instances,
                "调拨单撤回，在跑审批单已终止"
            );
        }
    }
    Ok(affected)
}

/// 提交审批前准备好审批单参数（**不写库**）。
///
/// - `Ok(None)`：没配已发布的调拨流程（降级：允许人工推 `APPROVED`）；
/// - `Err(409)`：已经有在跑的审批单（避免撞 `uk_biz_flow_instance_biz_active`）。
async fn prepare_start(
    row: &TransferRow,
    operator: Uuid,
) -> std::result::Result<Option<engine::StartReq>, ApiError> {
    let Some(flow) = transfer_model::published_transfer_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询调拨审批流程"))?
    else {
        tracing::debug!(
            transfer_id = %row.id,
            "没有配置 STOCK_TRANSFER 审批流程，提交后允许人工推 APPROVED（降级）"
        );
        return Ok(None);
    };
    if flow_model::biz_instance_running(&flow.biz_type, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(conflict(
            "该调拨单已有在跑的审批单：请等审批结束，或先撤回再重新提交",
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 库存调拨", row.transfer_no)
        .chars()
        .take(200)
        .collect::<String>();
    Ok(Some(engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code,
        flow_name: flow.flow_name,
        biz_type: flow.biz_type,
        biz_id: Some(row.id),
        biz_no: Some(row.transfer_no.clone()),
        title,
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: serde_json::json!({
            "transfer_id": row.id.to_string(),
            "transfer_no": row.transfer_no,
            "from_warehouse_id": row.from_warehouse_id.to_string(),
            "to_warehouse_id": row.to_warehouse_id.to_string(),
            "total_qty": row.total_qty.to_string(),
        }),
    }))
}

/// 状态流转核心逻辑（handler 与单测共用）。
///
/// 顺序：读单 → 目标态解析 → 状态机（非法边 409）→ 引用准备（在途仓 / 审批单）→
/// 一个事务里「发起审批 + 写状态（+ 撤回终止审批）」→ 回读。
pub async fn change_transfer_status(
    id: Uuid,
    req: &TransferStatusReq,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    let target = req.target_status()?;
    let cancel_reason = req.cancel_reason()?;
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let Some(effect) = transition_effect(&row.status, target) else {
        return Err(conflict(transition_rejected_message(&row.status, target)));
    };

    let mut transit_warehouse_id = None;
    if effect.needs_transit_warehouse {
        transit_warehouse_id = transfer_model::transit_warehouse_id()
            .await
            .map_err(|error| utils::db_error(error, "查询在途仓"))?;
        if transit_warehouse_id.is_none() {
            return Err(reference_missing(format!(
                "发货需要「在途仓」（warehouse_code = {}）：W3a 的 V0139 本应种好这一行，\
                 请检查 biz_warehouse 是否被删",
                transfer_model::TRANSIT_WAREHOUSE_CODE
            )));
        }
    }
    let start = if effect.status == STATUS_PENDING {
        prepare_start(&row, operator).await?
    } else {
        None
    };

    let now = Utc::now();
    let ctx = TransitionContext {
        id,
        from: &row.status,
        transfer_no: &row.transfer_no,
        effect: &effect,
        cancel_reason: cancel_reason.as_deref(),
        transit_warehouse_id,
        operator,
        now,
        start,
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    match apply_transition(&tx, &ctx).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "调拨单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.status
            )))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "调拨单状态流转"))
        }
    }
}

/// 状态流转（6 条合法边，其余 409）。
#[nik::permission(
    path = "/business/erp/transfer/{id}/status",
    method = "PATCH",
    permission = "business:erp:transfer:status"
)]
#[nik::patch("/business/erp/transfer/{id}/status")]
pub async fn transfer_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TransferStatusReq>,
) -> ResResult<TransferDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let operator = operator_of(&claims).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "无法从令牌解析当前用户：状态流转需要记录操作人（申请人 / 审批人）",
        )
    })?;
    change_transfer_status(id, &req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑥ 删除 ====================

/// 删除核心逻辑：**只有 `DRAFT` / `CANCELLED`** 能删（软删）。
pub async fn delete_transfer(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    if !matches!(
        row.status.as_str(),
        transfer_model::STATUS_DRAFT | transfer_model::STATUS_CANCELLED
    ) {
        return Err(conflict(format!(
            "调拨单当前是「{}」（{}），只有草稿（DRAFT）/ 已取消（CANCELLED）能删：\
             在途 / 已完成的单据是库存与审批的事实，不能删",
            transfer_model::status_label(&row.status),
            row.status
        )));
    }
    let affected = transfer_model::soft_delete(db(), id, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "软删调拨单"))?;
    if affected == 0 {
        return Err(not_found("调拨单"));
    }
    Ok(())
}

/// 删除调拨单（软删；仅 DRAFT / CANCELLED）。
#[nik::permission(
    path = "/business/erp/transfer/{id}",
    method = "DELETE",
    permission = "business:erp:transfer:delete"
)]
#[nik::delete("/business/erp/transfer/{id}")]
pub async fn transfer_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_transfer(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== ⑦ 序列号 ====================

/// 该调拨单涉及的所有序列号（按明细分组）。
pub async fn serials_of_transfer(id: Uuid) -> std::result::Result<TransferSerialsRes, ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let items = transfer_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨明细"))?;
    let serials = transfer_model::serials_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨序列号"))?;
    let mut by_item: std::collections::BTreeMap<Uuid, Vec<TransferSerialRes>> =
        std::collections::BTreeMap::new();
    for serial in &serials {
        by_item
            .entry(serial.item_id)
            .or_default()
            .push(TransferSerialRes::of(serial));
    }
    let total = serials.len();
    let groups = items
        .iter()
        .map(|item| TransferSerialGroupRes {
            item_id: item.id.to_string(),
            line_no: item.line_no,
            material_id: item.material_id.to_string(),
            serials: by_item.remove(&item.id).unwrap_or_default(),
        })
        .collect();
    Ok(TransferSerialsRes {
        transfer_id: row.id.to_string(),
        transfer_no: row.transfer_no,
        total,
        groups,
    })
}

/// 调拨单涉及的序列号（按明细分组）。
#[nik::permission(
    path = "/business/erp/transfer/{id}/serials",
    method = "GET",
    permission = "business:erp:transfer:detail"
)]
#[nik::get("/business/erp/transfer/{id}/serials")]
pub async fn transfer_serials(Path(id): Path<String>) -> ResResult<TransferSerialsRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(serials_of_transfer(id).await?))
}

// 编译期契约：状态常量必须覆盖 DDL 的 CHECK（漏一个 state_check_holds 就会恒 false）
const _: () = assert!(ALL_STATUSES.len() == 6);

/// 权限码对外只读引用（`scripts/verify/w3b_transfer_api.sh` 用它和 V0144 逐字比对）。
pub use crate::business::erp::transfer_dto::TRANSFER_PERMISSIONS as TRANSFER_PERMISSIONS_PUB;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::transfer_dto::TRANSFER_PERMISSIONS;

    /// 与 V0144 逐字一致的 6 个权限码（重复导出一次，单测与脚本都能拿到）。
    const _: [(&str, &str); 6] = TRANSFER_PERMISSIONS;

    #[test]
    fn permission_reexport_is_same_list() {
        assert_eq!(TRANSFER_PERMISSIONS_PUB.len(), 6);
        assert_eq!(TRANSFER_PERMISSIONS_PUB, TRANSFER_PERMISSIONS);
    }
}

// ==================== 单测：真库行为（卡面 §五 的 ①–⑰） ====================
//
// 为什么必须真库：本卡交付的是「6 态状态机真的能落库」「非 DRAFT 真的 409」
// 「提交审批真的回写 approval_instance_id」——纯逻辑单测证明不了
// 「`ck_biz_transfer_state` 的 5 个分支与 6 条边一一对上」这类必然 500 的错误。
// 所以这里走**生产同款路径**：
//
// ```text
// 夹具：仓 / 物料 / 序列号 / 在途仓（V0139 种子）
//   → transfer_handler::create_transfer / update_transfer / change_transfer_status / delete_transfer
//   → transfer_model 的 SQL 直接打到 cloudpivot_w3b（隔离库）
// ```
//
// 用法：
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_w3b'
// cargo test --lib business::erp::transfer_handler -- --nocapture --test-threads=1
// ```
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::erp::transfer_dto::{TRANSFER_PERMISSIONS, TransferItemReq};
    use crate::business::erp::transfer_model::{
        STATUS_APPROVED, STATUS_CANCELLED, STATUS_DONE, STATUS_DRAFT, STATUS_IN_TRANSIT,
        STATUS_PENDING,
    };
    use crate::business::flow::graph::{
        AssigneeKind, AssigneeRef, EdgeSpec, FlowGraph, NodeSpec, NodeType,
    };
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    // ---------- 夹具 UUID（固定值，与其他卡的夹具互不冲突） ----------
    const U_APPLICANT: &str = "d3b0000a-0000-4000-8000-000000000001";
    const U_APPROVER: &str = "d3b0000a-0000-4000-8000-000000000002";
    const WH_FROM: &str = "d3b0000b-0000-4000-8000-000000000001";
    const WH_TO: &str = "d3b0000b-0000-4000-8000-000000000002";
    const UNIT: &str = "d3b0000d-0000-4000-8000-000000000001";
    const M_PLAIN: &str = "d3b0000c-0000-4000-8000-000000000001";
    const M_SN: &str = "d3b0000c-0000-4000-8000-000000000002";
    const SERIAL_1: &str = "d3b0000e-0000-4000-8000-000000000001";
    const SERIAL_2: &str = "d3b0000e-0000-4000-8000-000000000002";
    const LOC_FROM: &str = "d3b0000f-0000-4000-8000-000000000001";
    const LOC_TO: &str = "d3b0000f-0000-4000-8000-000000000002";
    const DEF_ID: &str = "d3b0001a-0000-4000-8000-000000000001";
    const VER_ID: &str = "d3b0001a-0000-4000-8000-000000000002";

    /// 真库用例**串行**执行（共用一个库，取号器是全局单行，并行会互相推进号段）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 初始化全局 db（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // 池里的空闲连接绑在创建它的 runtime 上 → 必须逼每次新建（W1b 实测）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str, params: Vec<Value>, what: &str) {
        if let Err(err) = db().execute_with_params(sql, params).await {
            panic!("夹具写入失败（{what}）：{err}\nSQL: {sql}");
        }
    }

    /// 清掉本卡造的单据（按夹具仓库精确定位，不扫全库）。
    async fn cleanup() {
        let warehouses = vec![uuid(WH_FROM), uuid(WH_TO)];
        // ① 审批实例：按「本卡的流程定义」+「本卡的调拨单」两条口径一起删
        //    （上一轮跑完留下的实例若只按 biz_id 找不到，会挡住后面的流程版本删除）
        exec(
            "DELETE FROM biz_flow_instance \
              WHERE def_id = ?::uuid \
                 OR (biz_type = 'STOCK_TRANSFER' AND biz_id IN ( \
                       SELECT id FROM biz_stock_transfer WHERE from_warehouse_id = ANY(?::text::uuid[])))",
            vec![Value::from(&uuid(DEF_ID)), array_param(&warehouses)],
            "清理审批实例",
        )
        .await;
        exec(
            "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN ( \
                 SELECT i.id FROM biz_stock_transfer_item i \
                   JOIN biz_stock_transfer t ON t.id = i.transfer_id \
                  WHERE t.from_warehouse_id = ANY(?::text::uuid[]))",
            vec![array_param(&warehouses)],
            "清理序列号子表",
        )
        .await;
        exec(
            "DELETE FROM biz_stock_transfer_item WHERE transfer_id IN ( \
                 SELECT id FROM biz_stock_transfer WHERE from_warehouse_id = ANY(?::text::uuid[]))",
            vec![array_param(&warehouses)],
            "清理明细",
        )
        .await;
        exec(
            "DELETE FROM biz_stock_transfer WHERE from_warehouse_id = ANY(?::text::uuid[])",
            vec![array_param(&warehouses)],
            "清理单据头",
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理流程版本",
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def WHERE id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理流程定义",
        )
        .await;
    }

    /// UUID 数组参数（技能 §6.5：`Value::Text("{a,b}")` 配 `?::text::uuid[]`）。
    ///
    /// **花括号不能省**：PG 的数组字面量是 `{a,b}`，只写 `a,b` 会报
    /// `malformed array literal`（实测：`query failed: : db error`，没有更多细节）。
    fn array_param(ids: &[Uuid]) -> Value {
        Value::Text(format!(
            "{{{}}}",
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ))
    }

    async fn prepare() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        exec(
            "INSERT INTO sys_user (id, username, password) VALUES (?::uuid, 'w3b_applicant', 'x') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(U_APPLICANT))],
            "用户（申请人）",
        )
        .await;
        exec(
            "INSERT INTO sys_user (id, username, password) VALUES (?::uuid, 'w3b_approver', 'x') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(U_APPROVER))],
            "用户（审批人）",
        )
        .await;
        exec(
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order) \
             VALUES (?::uuid, 'W3B-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(UNIT))],
            "计量单位",
        )
        .await;
        // 注意：V0137 的 `allow_negative` / `is_leaf` 是 SMALLINT（0/1），不是 BOOLEAN
        // （`\\d` 实测；`serial_required` 反而是 BOOLEAN —— 同一批迁移里两种风格并存）
        exec(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                        allow_negative, status, sort_order, parent_id, level, \
                                        warehouse_path, is_leaf) \
             VALUES (?::uuid, 'WH-W3B-1', 'W3B 调出仓', 'NORMAL', 0, 1, 1, NULL, 1, '/w3b1/', 1), \
                    (?::uuid, 'WH-W3B-2', 'W3B 调入仓', 'NORMAL', 0, 1, 2, NULL, 1, '/w3b2/', 1) \
             ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(WH_FROM)), Value::from(&uuid(WH_TO))],
            "仓库",
        )
        .await;
        exec(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                       safety_stock, costing_method, tax_rate, status, serial_required) \
             VALUES (?::uuid, 'W3B-M-PLAIN', 'W3B 按数量物料', ?::uuid, 'RAW', 0, 'MOVING_AVG', 0, 1, false), \
                    (?::uuid, 'W3B-M-SN', 'W3B 一物一码物料', ?::uuid, 'FINISHED', 0, 'MOVING_AVG', 0, 1, true) \
             ON CONFLICT DO NOTHING",
            vec![
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(UNIT)),
                Value::from(&uuid(M_SN)),
                Value::from(&uuid(UNIT)),
            ],
            "物料",
        )
        .await;
        exec(
            "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status) \
             VALUES (?::uuid, 'W3B-SN-0001', ?::uuid, ?::uuid, 'IN_STOCK'), \
                    (?::uuid, 'W3B-SN-0002', ?::uuid, ?::uuid, 'IN_STOCK') \
             ON CONFLICT DO NOTHING",
            vec![
                Value::from(&uuid(SERIAL_1)),
                Value::from(&uuid(M_SN)),
                Value::from(&uuid(WH_FROM)),
                Value::from(&uuid(SERIAL_2)),
                Value::from(&uuid(M_SN)),
                Value::from(&uuid(WH_FROM)),
            ],
            "序列号",
        )
        .await;
        exec(
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) \
             VALUES (?::uuid, ?::uuid, ?::uuid, NULL, 0), (?::uuid, ?::uuid, ?::uuid, NULL, 0) \
             ON CONFLICT DO NOTHING",
            vec![
                Value::from(&uuid(LOC_FROM)),
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_FROM)),
                Value::from(&uuid(LOC_TO)),
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_TO)),
            ],
            "库位库存行",
        )
        .await;
        Some(guard)
    }

    /// 插入一条已发布的 `STOCK_TRANSFER` 流程（图用生产同款 `snapshot_json()` 序列化）。
    async fn publish_transfer_flow() {
        let mut start = NodeSpec::new("n_start", "开始", NodeType::Start);
        start.sort_order = 1;
        let mut approve = NodeSpec::new("n_approve", "仓管审批", NodeType::Approval);
        approve.assignee_type = Some(AssigneeKind::User);
        approve.assignees = vec![AssigneeRef {
            kind: "USER".to_string(),
            ref_id: U_APPROVER.to_string(),
            sort_order: 1,
        }];
        approve.empty_assignee = "ERROR".to_string();
        approve.sort_order = 2;
        let mut end = NodeSpec::new("n_end", "结束", NodeType::End);
        end.sort_order = 3;
        let graph = FlowGraph {
            nodes: vec![start, approve, end],
            edges: vec![
                EdgeSpec::new("e1", "n_start", "n_approve"),
                EdgeSpec::new("e2", "n_approve", "n_end"),
            ],
        };
        exec(
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status, \
                                       current_version_id, current_version_no, is_default) \
             VALUES (?::uuid, 'stock_transfer', '库存调拨审批', 'STOCK_TRANSFER', 'PUBLISHED', ?::uuid, 1, false)",
            vec![Value::from(&uuid(DEF_ID)), Value::from(&uuid(VER_ID))],
            "调拨流程定义",
        )
        .await;
        // ck_biz_flow_def_version_published：PUBLISHED 版本要求 graph_hash / graph_snapshot /
        // published_at 三者非空（`\d` 实测的约束定义，漏一个就 CheckViolation）
        exec(
            "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status, is_current, \
                                               graph_snapshot, graph_hash, node_count, edge_count, \
                                               published_at) \
             VALUES (?::uuid, ?::uuid, 1, 'PUBLISHED', true, ?::jsonb, ?, 3, 2, now())",
            vec![
                Value::from(&uuid(VER_ID)),
                Value::from(&uuid(DEF_ID)),
                Value::Json(graph.snapshot_json()),
                Value::Text(graph.graph_hash()),
            ],
            "调拨流程版本",
        )
        .await;
    }

    fn item(material_id: &str, qty: &str) -> TransferItemReq {
        TransferItemReq {
            material_id: material_id.to_string(),
            from_location_id: None,
            to_location_id: None,
            qty: Decimal::from_str(qty).expect("夹具数量"),
            serial_ids: Vec::new(),
        }
    }

    fn body(items: Vec<TransferItemReq>) -> CreateTransferReq {
        CreateTransferReq {
            from_warehouse_id: WH_FROM.to_string(),
            to_warehouse_id: WH_TO.to_string(),
            transfer_date: None,
            reason: Some("W3B 单测".to_string()),
            remark: None,
            items,
        }
    }

    fn status_req(status: &str, reason: Option<&str>) -> TransferStatusReq {
        TransferStatusReq {
            status: status.to_string(),
            reason: reason.map(str::to_string),
        }
    }

    async fn refresh(id: Uuid) -> TransferRow {
        transfer_model::find_by_id(id)
            .await
            .expect("查询调拨单")
            .expect("调拨单必须存在")
    }

    /// 建一张单并推进到指定状态（测试的公共前置）。
    async fn transfer_in_status(target: &str) -> (Uuid, TransferRow) {
        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(
            &body(vec![item(M_PLAIN, "5")]),
            Some(applicant),
            Some("w3b_applicant"),
        )
        .await
        .expect("建单必须成功");
        for step in [
            STATUS_PENDING,
            STATUS_APPROVED,
            STATUS_IN_TRANSIT,
            STATUS_DONE,
        ] {
            if target == STATUS_DRAFT {
                break;
            }
            let reason = if step == STATUS_CANCELLED {
                Some("单测取消")
            } else {
                None
            };
            change_transfer_status(id, &status_req(step, reason), applicant)
                .await
                .unwrap_or_else(|err| panic!("推进到 {step} 失败：{}", err.msg));
            if step == target {
                break;
            }
        }
        let row = refresh(id).await;
        (id, row)
    }

    // ---------- ① 建单：DRAFT + 单号格式 ----------

    #[tokio::test]
    async fn create_returns_draft_with_card_no_format() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_transfer(
            &body(vec![item(M_PLAIN, "5")]),
            Some(uuid(U_APPLICANT)),
            Some("w3b_applicant"),
        )
        .await
        .expect("建单必须成功");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_DRAFT);
        let parts: Vec<&str> = row.transfer_no.split('-').collect();
        assert_eq!(parts.len(), 3, "单号段数：{}", row.transfer_no);
        assert_eq!(parts[0], "TK");
        assert_eq!(parts[1].len(), 8, "日期段 8 位：{}", row.transfer_no);
        assert!(parts[1].chars().all(|ch| ch.is_ascii_digit()));
        assert_eq!(parts[2].len(), 6, "流水段 6 位：{}", row.transfer_no);
        assert_eq!(row.total_qty.to_string(), "5.0000");
        assert!(row.submitted_at.is_none(), "DRAFT 不能有 submitted_at");
        assert_eq!(row.applicant_id, Some(uuid(U_APPLICANT)));
    }

    // ---------- ② 建单：from = to 被 DB CHECK 拒 ----------

    #[tokio::test]
    async fn db_check_rejects_same_warehouse() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 绕过 DTO 校验，直接打模型层：证明 DB 的 ck_transfer_diff_wh 真的在
        let new = NewTransfer {
            from_warehouse_id: uuid(WH_FROM),
            to_warehouse_id: uuid(WH_FROM),
            transfer_date: None,
            reason: None,
            remark: None,
            applicant_id: Some(uuid(U_APPLICANT)),
            applicant_name: None,
        };
        let items = vec![NewItem {
            material_id: uuid(M_PLAIN),
            from_location_id: None,
            to_location_id: None,
            qty: Decimal::from_str("1").expect("qty"),
            serial_ids: Vec::new(),
        }];
        let err = transfer_model::create(
            db(),
            &new,
            &items,
            Decimal::from_str("1").expect("qty"),
            Some(uuid(U_APPLICANT)),
            Utc::now(),
        )
        .await
        .expect_err("同仓调拨必须被库约束拒绝");
        assert!(
            matches!(&err, DbError::CheckViolation { .. }),
            "应当是 CheckViolation（ck_transfer_diff_wh），实际：{err}"
        );
    }

    // ---------- ③ 建单：qty <= 0 / 明细为空（DTO 层 400） ----------

    #[test]
    fn create_validation_rejects_bad_qty_and_empty_items() {
        let zero = body(vec![item(M_PLAIN, "0")]);
        let err = zero.validate().expect_err("qty = 0 必须 400");
        assert_eq!(err.status, 400);
        let empty = body(Vec::new());
        let err = empty.validate().expect_err("空明细必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("items 不能为空"), "{}", err.msg);
    }

    // ---------- ④ 建单：序列号引用不合法（不是该物料 / 不在库）→ 400 ----------

    #[tokio::test]
    async fn create_rejects_foreign_or_outbound_serial() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 序列号属于 M_SN，却挂在 M_PLAIN 的行上 → 必须 400 而不是落库
        let mut line = item(M_PLAIN, "1");
        line.serial_ids = vec![SERIAL_1.to_string()];
        let err = create_transfer(&body(vec![line]), Some(uuid(U_APPLICANT)), None)
            .await
            .expect_err("序列号不属于该物料必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("在库"), "{}", err.msg);
    }

    // ---------- ⑤ 改单：DRAFT 能改（明细整体重建） ----------

    #[tokio::test]
    async fn update_rebuilds_items_for_draft() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
            .await
            .expect("建单");
        let mut second = item(M_SN, "2");
        second.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        update_transfer(id, &body(vec![item(M_PLAIN, "3"), second]), Some(applicant))
            .await
            .expect("DRAFT 改单必须成功");
        let detail = detail_of(id).await.expect("详情");
        assert_eq!(detail.items.len(), 2, "明细必须整体重建");
        assert_eq!(detail.items[1].line_no, 2);
        assert_eq!(detail.items[1].serial_ids.len(), 2);
        assert_eq!(detail.transfer.total_qty.to_string(), "5.0000");
    }

    // ---------- ⑥ 改单：PENDING 不能改 → 409 ----------

    #[tokio::test]
    async fn update_pending_conflicts() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (id, _) = transfer_in_status(STATUS_PENDING).await;
        let err = update_transfer(id, &body(vec![item(M_PLAIN, "9")]), Some(uuid(U_APPLICANT)))
            .await
            .expect_err("PENDING 改单必须 409");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
        // 明细没被改动
        let detail = detail_of(id).await.expect("详情");
        assert_eq!(detail.transfer.total_qty.to_string(), "5.0000");
    }

    // ---------- ⑦ 状态机：DRAFT → PENDING ----------

    #[tokio::test]
    async fn draft_to_pending_writes_submitted_at() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (_, row) = transfer_in_status(STATUS_PENDING).await;
        assert_eq!(row.status, STATUS_PENDING);
        assert!(row.submitted_at.is_some(), "PENDING 必须有 submitted_at");
        assert!(row.approved_at.is_none());
        // 「配了流程就回写 approval_instance_id」由 submit_starts_… 钉住；
        // 这里**不**断言「没有实例」——同一个测试库里可能还留着别的卡种下的
        // STOCK_TRANSFER 流程（e2e 脚本会发一条），那种环境下提交本来就该发起审批。
        assert!(
            row.approval_instance_id.is_some()
                || transfer_model::published_transfer_flow()
                    .await
                    .expect("查询已发布流程")
                    .is_none(),
            "没配流程时不该有审批单"
        );
    }

    // ---------- ⑧ 状态机：DRAFT → DONE 跳级 → 409 ----------

    #[tokio::test]
    async fn draft_to_done_is_rejected() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_transfer(
            &body(vec![item(M_PLAIN, "1")]),
            Some(uuid(U_APPLICANT)),
            None,
        )
        .await
        .expect("建单");
        let err = change_transfer_status(id, &status_req(STATUS_DONE, None), uuid(U_APPLICANT))
            .await
            .expect_err("跳级必须 409");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("非法状态流转"), "{}", err.msg);
        assert_eq!(refresh(id).await.status, STATUS_DRAFT, "状态不得被改动");
    }

    // ---------- ⑨ 状态机：PENDING → APPROVED ----------

    #[tokio::test]
    async fn pending_to_approved_writes_approved_at() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (_, row) = transfer_in_status(STATUS_APPROVED).await;
        assert_eq!(row.status, STATUS_APPROVED);
        assert!(row.submitted_at.is_some());
        assert!(row.approved_at.is_some());
        assert_eq!(row.approver_id, Some(uuid(U_APPLICANT)));
    }

    // ---------- ⑩ 状态机：PENDING → CANCELLED（撤回） ----------

    #[tokio::test]
    async fn pending_to_cancelled_requires_reason() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (id, _) = transfer_in_status(STATUS_PENDING).await;
        // 不给原因 → 400（库约束要求 cancel_reason 非空白）
        let err =
            change_transfer_status(id, &status_req(STATUS_CANCELLED, None), uuid(U_APPLICANT))
                .await
                .expect_err("撤回必须给原因");
        assert_eq!(err.status, 400);
        // 给原因 → CANCELLED
        change_transfer_status(
            id,
            &status_req(STATUS_CANCELLED, Some("需求取消")),
            uuid(U_APPLICANT),
        )
        .await
        .expect("撤回必须成功");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_CANCELLED);
        assert_eq!(row.cancel_reason.as_deref(), Some("需求取消"));
        assert!(row.cancelled_at.is_some());
        assert!(
            row.submitted_at.is_some(),
            "撤回不清理提交时间（CANCELLED 分支不约束）"
        );
    }

    // ---------- ⑪ 状态机：APPROVED → IN_TRANSIT（回填在途仓） ----------

    #[tokio::test]
    async fn approved_to_in_transit_fills_transit_warehouse() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (_, row) = transfer_in_status(STATUS_IN_TRANSIT).await;
        assert_eq!(row.status, STATUS_IN_TRANSIT);
        assert!(row.shipped_at.is_some());
        let transit = transfer_model::transit_warehouse_id()
            .await
            .expect("查询在途仓")
            .expect("V0139 必须种好 WH-TRANSIT");
        assert_eq!(row.transit_warehouse_id, Some(transit));
    }

    // ---------- ⑫ 状态机：IN_TRANSIT → DONE ----------

    #[tokio::test]
    async fn in_transit_to_done_writes_received_at() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (_, row) = transfer_in_status(STATUS_DONE).await;
        assert_eq!(row.status, STATUS_DONE);
        assert!(row.received_at.is_some());
        // 本卡不动库存（W3c 的活）：明细的 shipped_qty / received_qty 仍是 0
        let items = transfer_model::items_of(row.id).await.expect("明细");
        assert_eq!(items[0].shipped_qty.to_string(), "0.0000");
        assert_eq!(items[0].received_qty.to_string(), "0.0000");
    }

    // ---------- ⑬ 状态机：DONE 是终态（任何目标 409） ----------

    #[tokio::test]
    async fn done_rejects_every_transition() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (id, _) = transfer_in_status(STATUS_DONE).await;
        for target in [STATUS_DRAFT, STATUS_CANCELLED, STATUS_PENDING] {
            let err = change_transfer_status(
                id,
                &status_req(target, Some("终态试探")),
                uuid(U_APPLICANT),
            )
            .await
            .expect_err("DONE 之后任何流转都必须 409");
            assert_eq!(err.status, 409, "目标 {target}");
        }
        assert_eq!(refresh(id).await.status, STATUS_DONE);
    }

    // ---------- ⑭ / ⑮ 删除：DRAFT 能删、IN_TRANSIT 不能删 ----------

    #[tokio::test]
    async fn delete_allows_draft_and_rejects_in_transit() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let applicant = uuid(U_APPLICANT);
        let draft = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
            .await
            .expect("建单");
        delete_transfer(draft, Some(applicant))
            .await
            .expect("DRAFT 必须能删");
        assert!(
            transfer_model::find_by_id(draft)
                .await
                .expect("查询")
                .is_none(),
            "软删后按主键必须查不到"
        );

        let (transit, _) = transfer_in_status(STATUS_IN_TRANSIT).await;
        let err = delete_transfer(transit, Some(applicant))
            .await
            .expect_err("IN_TRANSIT 不能删");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("在途"), "{}", err.msg);
        assert_eq!(refresh(transit).await.status, STATUS_IN_TRANSIT);
    }

    // ---------- ⑯ 审批：有流程时提交即生成 approval_instance_id ----------

    #[tokio::test]
    async fn submit_starts_approval_instance_when_flow_published() {
        let Some(_guard) = prepare().await else {
            return;
        };
        publish_transfer_flow().await;
        let (_, row) = transfer_in_status(STATUS_PENDING).await;
        let instance_id = row
            .approval_instance_id
            .expect("配了已发布的 STOCK_TRANSFER 流程就必须回写 approval_instance_id");
        let instance = flow_model::find_instance(instance_id)
            .await
            .expect("查询审批实例")
            .expect("审批实例必须存在");
        assert_eq!(instance.biz_type, "STOCK_TRANSFER");
        assert_eq!(instance.biz_id, Some(row.id));
        assert_eq!(instance.biz_no.as_deref(), Some(row.transfer_no.as_str()));
        assert_eq!(instance.status, "PENDING", "审批人未处理前必须是 PENDING");
    }

    // ---------- ⑰ 审批回调：APPROVED / REJECTED ----------

    #[tokio::test]
    async fn approval_callback_moves_transfer() {
        let Some(_guard) = prepare().await else {
            return;
        };
        publish_transfer_flow().await;
        let applicant = uuid(U_APPLICANT);

        // APPROVED → 调拨单 APPROVED
        let (approved_id, approved_row) = transfer_in_status(STATUS_PENDING).await;
        let instance_id = approved_row.approval_instance_id.expect("审批实例 id");
        crate::business::flow::task_bridge::on_transfer_settled(Some(instance_id), "APPROVED")
            .await;
        let row = refresh(approved_id).await;
        assert_eq!(row.status, STATUS_APPROVED);
        assert!(row.approved_at.is_some());

        // REJECTED → 打回 DRAFT（审批意见写 reason，时间戳清空）
        let rejected_id = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
            .await
            .expect("建单");
        change_transfer_status(rejected_id, &status_req(STATUS_PENDING, None), applicant)
            .await
            .expect("提交审批");
        let rejected_row = refresh(rejected_id).await;
        let instance_id = rejected_row.approval_instance_id.expect("审批实例 id");
        // 引擎把驳回意见写在待办行；这里直接断言「意见进了 reason」这条链路
        db().execute_with_params(
            "UPDATE biz_flow_instance SET result_comment = '不同意，数量再核' WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("写审批意见");
        crate::business::flow::task_bridge::on_transfer_settled(Some(instance_id), "REJECTED")
            .await;
        let row = refresh(rejected_id).await;
        assert_eq!(row.status, STATUS_DRAFT, "驳回必须打回 DRAFT");
        assert!(row.submitted_at.is_none(), "DRAFT 分支要求时间戳全空");
        assert!(row.approved_at.is_none());
        assert_eq!(row.reason.as_deref(), Some("不同意，数量再核"));
        assert!(row.approval_instance_id.is_none(), "打回后要能重新提交");
    }

    // ---------- ⑱ 详情 / 序列号分组 ----------

    #[tokio::test]
    async fn detail_and_serials_are_grouped_by_line() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let mut sn_line = item(M_SN, "2");
        sn_line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        let id = create_transfer(
            &body(vec![item(M_PLAIN, "1"), sn_line]),
            Some(uuid(U_APPLICANT)),
            None,
        )
        .await
        .expect("建单");
        let detail = detail_of(id).await.expect("详情");
        assert_eq!(detail.items.len(), 2);
        assert_eq!(detail.items[0].serial_ids.len(), 0);
        assert_eq!(detail.items[1].serial_ids.len(), 2);
        assert_eq!(detail.items[1].material_code.as_deref(), Some("W3B-M-SN"));

        let serials = serials_of_transfer(id).await.expect("序列号分组");
        assert_eq!(serials.total, 2);
        assert_eq!(serials.groups.len(), 2, "两行都要出现（空行也是空分组）");
        let sn_group = serials
            .groups
            .iter()
            .find(|group| group.line_no == 2)
            .expect("第 2 行的分组");
        assert_eq!(sn_group.serials.len(), 2);
        assert!(
            sn_group.serials.iter().all(|serial| serial
                .serial_no
                .as_deref()
                .unwrap_or("")
                .starts_with("W3B-SN-")),
            "必须回读出序列号文本"
        );
    }

    // ---------- ⑲ 列表：状态 / 仓库筛选 + 分页 ----------

    #[tokio::test]
    async fn page_filters_by_status_and_warehouse() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let applicant = uuid(U_APPLICANT);
        let pending = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
            .await
            .expect("建单");
        change_transfer_status(pending, &status_req(STATUS_PENDING, None), applicant)
            .await
            .expect("提交审批");
        create_transfer(&body(vec![item(M_PLAIN, "2")]), Some(applicant), None)
            .await
            .expect("建单");

        let query = TransferQuery {
            status: Some(STATUS_PENDING.to_string()),
            from_warehouse_id: Some(uuid(WH_FROM)),
            to_warehouse_id: Some(uuid(WH_TO)),
            transfer_no: None,
            page: 1,
            page_size: 50,
        };
        let (rows, total) = transfer_model::page(&query).await.expect("分页");
        assert!(total >= 1);
        assert!(
            rows.iter().all(|row| row.status == STATUS_PENDING),
            "状态筛选必须生效"
        );
        assert!(
            rows.iter()
                .all(|row| row.from_warehouse_id == uuid(WH_FROM)),
            "仓库筛选必须生效"
        );
        // 单号前缀筛选能命中自己
        let no = refresh(pending).await.transfer_no;
        let query = TransferQuery {
            from_warehouse_id: Some(uuid(WH_FROM)),
            transfer_no: Some(no.clone()),
            page: 1,
            page_size: 10,
            ..Default::default()
        };
        let (rows, _) = transfer_model::page(&query).await.expect("分页");
        assert_eq!(rows.len(), 1, "单号前缀应当唯一命中：{no}");
        assert_eq!(rows[0].transfer_no, no);
    }

    // ---------- ⑳ 权限码：6 条与 V0144 逐字一致（真 403 见 verify 脚本） ----------

    #[test]
    fn permission_codes_are_six_and_stable() {
        assert_eq!(TRANSFER_PERMISSIONS.len(), 6);
        for (code, _) in TRANSFER_PERMISSIONS {
            assert!(code.starts_with("business:erp:transfer:"), "{code}");
        }
    }
}
