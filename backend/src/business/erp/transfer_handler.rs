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
//! # 库存双写（W3c）
//!
//! `IN_TRANSIT → DONE` 这一步在**同一个事务**里多做四件事（[`transfer_model::settle_transfer`]）：
//!
//! ```text
//! ① 调出仓 qty -= 数量（条件扣减 AND qty >= ?，防并发超卖）+ 流水 OUT / TRANSFER_OUT
//! ② 调入仓 qty += 数量（无库存行则建档）+ 流水 IN / TRANSFER_IN
//! ③ 一物一码物料：序列号 warehouse_id / location_id 跟着走（数量物料一个序列号都不动）
//! ④ 成本原价转移：unit_cost = 调出仓 avg_cost 快照；调入仓移动加权重算；金额守恒
//! ```
//!
//! 结算失败（库存不足 / 序列号不符 / 不变式不满足）→ **整笔回滚**，调拨单仍是 `IN_TRANSIT`，
//! 不会出现「状态已 DONE、库存没动」这种对不上的账。状态 CAS（`WHERE status = 'IN_TRANSIT'`）
//! 先执行，所以同一张单被并发推两次时只有一个事务能走到结算（另一个拿 409），
//! 单号级别的重复调用也不会二次扣库存（`settle_transfer` 还有一道流水幂等闸）。

use axum::Json;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::transfer_dto::{
    CreateTransferReq, TransferDetailRes, TransferFilter, TransferItemRes, TransferRes,
    TransferSerialGroupRes, TransferSerialRes, TransferSerialsRes, TransferStatusReq,
};
use crate::business::erp::transfer_model::{
    self, ALL_STATUSES, NewItem, NewTransfer, STATUS_PENDING, SettleError, TransferQuery,
    TransferRow, transition_effect, transition_rejected_message,
};
use crate::business::flow::{engine, model as flow_model, task_bridge as flow_task_bridge};
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

/// 结算失败 → HTTP：库存不足 409、序列号不符 400、不变式不满足 409。
///
/// 为什么不复用 [`write_error`]：结算的失败原因里有一半**不是数据库错误**
/// （库存不足是业务冲突、序列号个数不匹配是参数问题），必须给出能直接照着改的文案，
/// 而不是笼统的「HTTP 500 服务内部错误」。
fn settle_error(err: SettleError) -> ApiError {
    match err {
        SettleError::Db(inner) => write_error(inner, "调拨结算（库存双写）"),
        SettleError::Insufficient {
            material_code,
            required,
            available,
        } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "调出仓库存不足：物料 {material_code} 本次需要 {required}，当前可用 {available}。\
                 另一张调拨单可能刚扣走同一批库存，请刷新库存后重新提交"
            ),
        ),
        SettleError::SerialMismatch {
            line_no,
            qty,
            serials,
        } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "第 {line_no} 行是一物一码物料：明细数量 {qty} 与建单时的序列号个数 {serials} 不一致，\
                 请改单重新选择序列号"
            ),
        ),
        SettleError::SerialNotInSource {
            line_no,
            expected,
            moved,
        } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "第 {line_no} 行的序列号当前不在调出仓或已不在库（应有 {expected} 个，实际只转移了 \
                 {moved} 个）：请核对序列号是否已被其他单据出库"
            ),
        ),
        SettleError::Invariant(message) => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!("调拨结算后不变式校验失败：{message}"),
        ),
        SettleError::EmptyItems => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "调拨单没有明细，无法结算".to_string(),
        ),
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
    /// 调出仓库（`DONE` 时结算要读它：库存扣减 + 流水挂仓）。
    from_warehouse_id: Uuid,
    /// 调入仓库（`DONE` 时结算要读它：库存增加 + 流水挂仓）。
    to_warehouse_id: Uuid,
    /// 提交审批时要发起的审批单参数（`None` = 没配流程 / 其它目标态）。
    start: Option<engine::StartReq>,
}

/// 事务内的状态写入（`affected = 0` 表示 CAS 失败，由调用方翻 409）。
///
/// `IN_TRANSIT → DONE` 时**同事务**再做库存双写（[`transfer_model::settle_transfer`]）：
/// 结算失败就把整笔（含刚写的 `DONE`）一起回滚，调拨单仍是 `IN_TRANSIT`。
async fn apply_transition(
    tx: &Transaction,
    ctx: &TransitionContext<'_>,
) -> std::result::Result<u64, SettleError> {
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
    // 收货完成：库存双写 + 成本原价转移（**同一事务**，失败整笔回滚，状态留在 IN_TRANSIT）
    if affected > 0 && ctx.effect.status == transfer_model::STATUS_DONE {
        let outcome = transfer_model::settle_transfer(
            tx,
            ctx.id,
            ctx.transfer_no,
            ctx.from_warehouse_id,
            ctx.to_warehouse_id,
            Some(ctx.operator),
            ctx.now,
        )
        .await?;
        if outcome.settled {
            tracing::info!(
                transfer_id = %ctx.id,
                transfer_no = %ctx.transfer_no,
                lines = outcome.lines,
                qty = %outcome.qty,
                amount = %outcome.amount,
                serials = outcome.serials,
                "调拨单收货完成：库存双写 + 成本原价转移成功"
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

    // ⓘ 审批门闸（P5）：挂着在跑的审批单时，人工不许把单据直推 APPROVED（409 + 65030）。
    //   判据是纯函数 `flow_gate_blocked`（三条同时成立才拦），实例状态从审批单现查 ——
    //   单据行的 `approval_instance_id` 是弱引用（无外键），查不到实例就按「不拦」处理
    //   （与 D3a 的任务门闸同口径：读不到状态时宁可放行，也不要把单据锁死）。
    let instance_status = flow_task_bridge::instance_status_of(row.approval_instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单审批单状态"))?;
    if transfer_model::flow_gate_blocked(
        &row.status,
        target,
        row.approval_instance_id,
        instance_status.as_deref(),
    ) {
        tracing::info!(
            transfer_id = %row.id,
            transfer_no = %row.transfer_no,
            approval_instance_id = ?row.approval_instance_id,
            instance_status = ?instance_status,
            operator = %operator,
            "调拨单审批中，人工直推 APPROVED 被门闸拦截（65030）"
        );
        return Err(ApiError::new(
            409,
            error_code::TRANSFER_FLOW_GATE_BLOCKED,
            transfer_model::FLOW_GATE_BLOCKED_MESSAGE,
        ));
    }

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
        from_warehouse_id: row.from_warehouse_id,
        to_warehouse_id: row.to_warehouse_id,
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
        Err(SettleError::Db(error)) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "调拨单状态流转"))
        }
        Err(other) => {
            let _ = tx.rollback().await;
            let api = settle_error(other);
            tracing::warn!(
                transfer_id = %id,
                transfer_no = %row.transfer_no,
                status = %api.status,
                code = api.code,
                msg = %api.msg,
                "调拨单结算失败，已整笔回滚（状态仍为 IN_TRANSIT）"
            );
            Err(api)
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

// ==================== 单测夹具：走审批主线让单据进 APPROVED ====================

/// 让一张调拨单的审批单**真的落终态**（引擎在事务里投票 → 提交 → 回调调拨单）。
///
/// P5 起 `PENDING → APPROVED` 有人工门闸（`65030`），夹具不能再靠「人工直推」跨越审批，
/// 只能走生产路径。两套真库用例（`db_tests` / `w3c_tests`）共用这一个实现，
/// 免得两边对「怎么样才算审批通过」各写一套。
#[cfg(test)]
async fn approve_flow_instance(instance_id: Uuid) {
    let todos = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    let todo = todos.first().expect("审批通过前必须有 PENDING 待办");
    let task_id: Uuid = todo.get_typed("id").expect("todo id");
    let approver: Uuid = todo.get_typed("assignee_id").expect("assignee");
    let tx = db().begin().await.expect("开审批事务");
    let out = engine::approve(&tx, task_id, approver, Some("单测：同意"))
        .await
        .expect("引擎同意失败");
    tx.commit().await.expect("提交审批事务");
    crate::business::flow::task_bridge::on_transfer_settled(out.instance_id, &out.instance_status)
        .await;
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
    use crate::business::flow::graph::{canonical_json, parse_snapshot};
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
    // P5 起调拨流程是**迁移种的**（V0157），固定 UUID 与迁移里一致：
    // 单测断言直接按这两个 id 定位（不按 flow_code 模糊匹配，避免同业务类型的其它夹具串味）。
    const DEF_ID: &str = "20000000-0000-4000-8000-00000000d157";
    const VER_ID: &str = "20000000-0000-4000-8000-00000000e157";

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
        // 注：**不删流程定义** —— P5 起调拨流程由迁移 V0157 种好（`stock_transfer`），
        // 测试直接用它（生产同款路径）。夹具只清本卡造的单据与它们的审批单。
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
        // W3c：库存台账夹具 —— 每个用例都重置到同一基线，断言才能算增量
        //   M_PLAIN 分仓 100 @12.5 / 总仓 10 @8；M_SN 分仓 2 @30（SERIAL_1/2 都在分仓 IN_STOCK）
        let warehouses = vec![uuid(WH_FROM), uuid(WH_TO)];
        exec(
            "DELETE FROM biz_inventory_txn WHERE warehouse_id = ANY(?::text::uuid[])",
            vec![array_param(&warehouses)],
            "重置库存流水",
        )
        .await;
        exec(
            "DELETE FROM biz_inventory WHERE warehouse_id = ANY(?::text::uuid[])",
            vec![array_param(&warehouses)],
            "重置库存台账",
        )
        .await;
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                        avg_cost, amount, last_txn_at) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 100, 0, 12.5, 1250, now()), \
                    (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 10, 0, 8, 80, now()), \
                    (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 2, 0, 30, 60, now())",
            vec![
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_FROM)),
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_TO)),
                Value::from(&uuid(M_SN)),
                Value::from(&uuid(WH_FROM)),
            ],
            "库存台账基线",
        )
        .await;
        // 序列号也要回到「分仓 + IN_STOCK」：上一个用例可能把它们调去总仓（ON CONFLICT 不会覆盖）
        exec(
            "UPDATE biz_material_serial \
                SET warehouse_id = ?::uuid, location_id = NULL, status = 'IN_STOCK', \
                    updated_at = now() \
              WHERE id = ANY(?::text::uuid[])",
            vec![
                Value::from(&uuid(WH_FROM)),
                array_param(&[uuid(SERIAL_1), uuid(SERIAL_2)]),
            ],
            "重置序列号位置",
        )
        .await;
        // ⑥ 的状态（调拨流程）也在这里自愈：上一个用例若 panic 没恢复软删，后续用例会串味
        ensure_seed_flow().await;
        Some(guard)
    }

    /// 断言「迁移 V0157 已经把调拨流程种好」，并返回 `(def_id, version_id)`。
    ///
    /// P5 起**不再由夹具造流程**：调拨流程是迁移种子（生产同款路径），被测代码读的就是它。
    /// 夹具只负责「把可能被别的用例改坏的状态复原」：
    /// - 上一次 ⑥ 用例把流程软删后若 panic 没来得及恢复 → 这里 `deleted_at = NULL` 自愈；
    /// - 流程被改成非 PUBLISHED（不该发生，但自愈一次代价极低）→ 复原。
    ///
    /// # Panics
    ///
    /// 迁移没应用（测试库停在 V0156 之前）→ 直接 panic 并给出可操作的提示。
    async fn ensure_seed_flow() -> (Uuid, Uuid) {
        exec(
            "UPDATE biz_flow_def SET status = 'PUBLISHED', is_default = TRUE, deleted_at = NULL, \
                                    current_version_id = ?::uuid, current_version_no = 1 \
              WHERE id = ?::uuid",
            vec![Value::from(&uuid(VER_ID)), Value::from(&uuid(DEF_ID))],
            "自愈：调拨种子流程状态",
        )
        .await;
        let rows = db()
            .find_with_params(
                "SELECT d.status, d.is_default, d.biz_type, d.current_version_id, v.version_status \
                   FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
                  WHERE d.id = ?::uuid AND d.deleted_at IS NULL",
                vec![Value::from(&uuid(DEF_ID))],
            )
            .await
            .expect("查询调拨种子流程");
        let row = rows.first().unwrap_or_else(|| {
            panic!(
                "测试库缺少 stock_transfer 流程（V0157 未应用？）：\
                 先对测试库跑一次 `target/debug/cloudpivot`（迁移自动应用），再重跑本套用例"
            )
        });
        assert_eq!(
            row.get_typed::<String>("status").expect("status"),
            "PUBLISHED",
            "① 种子流程必须是 PUBLISHED"
        );
        assert!(
            row.get_typed::<bool>("is_default").expect("is_default"),
            "① 种子流程必须是 is_default（提交审批时按 biz_type 取默认流程）"
        );
        assert_eq!(
            row.get_typed::<String>("biz_type").expect("biz_type"),
            "STOCK_TRANSFER"
        );
        assert_eq!(
            row.get_typed::<String>("version_status")
                .expect("version_status"),
            "PUBLISHED",
            "① 当前版本必须是 PUBLISHED"
        );
        (uuid(DEF_ID), uuid(VER_ID))
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

    /// sha256 小写十六进制（与 `graph::graph_hash` 同口径：对 canonical JSON 取 digest）。
    fn hex_sha256(text: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(text.as_bytes());
        let mut out = String::with_capacity(64);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// 某物料在某仓的整仓库存数量（`location_id IS NULL` 的汇总桶），返回库里的 `numeric::text`。
    async fn inventory_qty(material_id: &str, warehouse_id: &str) -> String {
        let rows = db()
            .find_with_params(
                "SELECT qty::text AS qty FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
                vec![
                    Value::from(&uuid(material_id)),
                    Value::from(&uuid(warehouse_id)),
                ],
            )
            .await
            .expect("查库存数量");
        rows.first()
            .expect("库存行必须存在")
            .get_typed("qty")
            .expect("取 qty")
    }

    /// 某物料在某仓的整仓库存金额。
    async fn inventory_amount(material_id: &str, warehouse_id: &str) -> String {
        let rows = db()
            .find_with_params(
                "SELECT amount::text AS amount FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
                vec![
                    Value::from(&uuid(material_id)),
                    Value::from(&uuid(warehouse_id)),
                ],
            )
            .await
            .expect("查库存金额");
        rows.first()
            .expect("库存行必须存在")
            .get_typed("amount")
            .expect("取 amount")
    }

    /// 数值比较（`NUMERIC(18,4)` 经 text 回来是 `95.0000`，逐字比字符串会假红 —— W3c 的教训）。
    fn assert_dec_eq(actual: String, expected: &str, what: &str) {
        let left = Decimal::from_str(&actual)
            .unwrap_or_else(|_| panic!("{what}：实际值不是数字 [{actual}]"));
        let right = Decimal::from_str(expected)
            .unwrap_or_else(|_| panic!("{what}：期望值非法 [{expected}]"));
        assert_eq!(left, right, "{what}：期望 {expected} 实际 {actual}");
    }

    /// 走**生产同款路径**让审批单落终态：引擎在事务里投票 → 提交 → 回调调拨单。
    ///
    /// P5 之前夹具可以「人工直推 APPROVED」把单据推过审批；现在那条路被门闸（65030）拦住了，
    /// 夹具必须真的去审（也正好把「审批通过 → 调拨单 APPROVED」这条主路径跑在单测里）。
    /// 实现见文件级 `approve_flow_instance`（两套真库用例共用）。
    async fn approve_instance(instance_id: Uuid) {
        super::approve_flow_instance(instance_id).await;
    }

    /// 建一张单并推进到指定状态（测试的公共前置）。
    ///
    /// `PENDING → APPROVED` 这一步：**挂了审批单就走审批回调**（门闸 65030 不许人工直推），
    /// 没挂审批单（降级路径）才人工推。
    async fn transfer_in_status(target: &str) -> (Uuid, TransferRow) {
        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(
            &body(vec![item(M_PLAIN, "5")]),
            Some(applicant),
            Some("w3b_applicant"),
        )
        .await
        .expect("建单必须成功");
        if target == STATUS_DRAFT {
            return (id, refresh(id).await);
        }
        change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
            .await
            .unwrap_or_else(|err| panic!("推进到 {STATUS_PENDING} 失败：{}", err.msg));
        if target == STATUS_PENDING {
            return (id, refresh(id).await);
        }
        // → APPROVED
        match refresh(id).await.approval_instance_id {
            Some(instance_id) => approve_instance(instance_id).await,
            None => {
                change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
                    .await
                    .unwrap_or_else(|err| panic!("降级路径推进到 APPROVED 失败：{}", err.msg));
            }
        }
        for step in [STATUS_IN_TRANSIT, STATUS_DONE] {
            if target == STATUS_APPROVED {
                break;
            }
            change_transfer_status(id, &status_req(step, None), applicant)
                .await
                .unwrap_or_else(|err| panic!("推进到 {step} 失败：{}", err.msg));
            if step == target {
                break;
            }
        }
        (id, refresh(id).await)
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

    // ---------- ⑨ 状态机：PENDING → APPROVED（走审批主线） ----------

    #[tokio::test]
    async fn pending_to_approved_writes_approved_at() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (_, row) = transfer_in_status(STATUS_APPROVED).await;
        assert_eq!(row.status, STATUS_APPROVED);
        assert!(row.submitted_at.is_some());
        assert!(row.approved_at.is_some());
        // P5 起这条边只由**审批回调**走：approver_id 记的是点「同意」的那个审批人
        // （不是申请人自己），人工直推已被门闸 65030 拦住
        assert!(
            row.approver_id.is_some(),
            "审批通过要在 approver_id 记实际审批人（approval_callback 从待办取）"
        );
        assert_ne!(
            row.approver_id,
            Some(uuid(U_APPLICANT)),
            "审批人不是申请人本人（自审策略 ALLOW 也要求走审批待办）"
        );
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
        // W3c 起：DONE 会结算库存，明细的 shipped_qty / received_qty 都要回填成 qty
        // （库存侧的断言见 w3c_settle_* 系列用例）
        let items = transfer_model::items_of(row.id).await.expect("明细");
        assert_eq!(items[0].shipped_qty, items[0].qty);
        assert_eq!(items[0].received_qty, items[0].qty);
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
        ensure_seed_flow().await;
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
        ensure_seed_flow().await;
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

    // ---------- ⑰-b（P5 ①）迁移种的流程：PUBLISHED + 默认 + 图 hash 自洽 ----------
    //
    // 证明的是「种子迁移与引擎的自检口径一致」：`maintain.rs` 每天用
    // `hash(发布快照)` 与 `hash(节点/边行)` 对账 `graph_hash`，种子若有一处对不上，
    // 巡检会天天报 drift。这里把快照重新 canonical 化后重算 sha256，与落库的 `graph_hash` 比对。

    #[tokio::test]
    async fn seed_flow_is_published_with_self_consistent_hash() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (def_id, version_id) = ensure_seed_flow().await;

        // ① 定义行：存在 / 编码 / 业务类型 / 默认
        let defs = db()
            .find_with_params(
                "SELECT flow_code, biz_type, status, is_default FROM biz_flow_def \
                  WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&def_id)],
            )
            .await
            .expect("查流程定义");
        let def = defs
            .first()
            .expect("① 迁移种后 biz_flow_def 必须有 stock_transfer");
        assert_eq!(
            def.get_typed::<String>("flow_code").expect("code"),
            "stock_transfer"
        );
        assert_eq!(
            def.get_typed::<String>("biz_type").expect("biz_type"),
            "STOCK_TRANSFER"
        );

        // ① 版本行：PUBLISHED + 快照/hash/发布时间 + 计数
        let versions = db()
            .find_with_params(
                "SELECT version_status, graph_hash, node_count, edge_count, \
                        graph_snapshot::text AS snapshot_text \
                   FROM biz_flow_def_version WHERE id = ?::uuid",
                vec![Value::from(&version_id)],
            )
            .await
            .expect("查流程版本");
        let version = versions.first().expect("① 迁移种后必须有版本行");
        assert_eq!(
            version
                .get_typed::<String>("version_status")
                .expect("status"),
            "PUBLISHED"
        );
        assert_eq!(version.get_typed::<i32>("node_count").expect("nodes"), 3);
        assert_eq!(version.get_typed::<i32>("edge_count").expect("edges"), 2);

        // ① 图内容：快照能被引擎解析，且重算 hash == 落库 hash
        let text: String = version.get_typed("snapshot_text").expect("snapshot");
        let snapshot = parse_snapshot(&text).expect("① 快照必须能被 graph::parse_snapshot 解析");
        let recomputed = hex_sha256(&canonical_json(
            &serde_json::to_value(&snapshot).expect("快照序列化"),
        ));
        assert_eq!(
            recomputed,
            version.get_typed::<String>("graph_hash").expect("hash"),
            "① 快照重算的 sha256 必须等于落库 graph_hash（否则 D4d 每日自检会报 drift）"
        );
        let keys: Vec<&str> = snapshot.nodes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["e", "n_approve", "s"],
            "① 节点按 key 升序（canonical 前提）"
        );
        let approver = snapshot
            .nodes
            .iter()
            .find(|node| node.key == "n_approve")
            .expect("审批节点");
        assert_eq!(approver.node_type, "APPROVAL");
        assert_eq!(
            approver.assignee_type.as_deref(),
            Some("ADMIN"),
            "① 审批人来源 = ADMIN（超管），保证任何环境都有人收得到待办"
        );

        // ① 行侧与快照侧一致（node/edge 行是编辑器读的来源，不一致会让「打开流程图」看到旧图）
        let node_rows: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_node WHERE version_id = ?::uuid",
                vec![Value::from(&version_id)],
            )
            .await
            .expect("查节点行数");
        assert_eq!(node_rows, 3, "① 节点行 3 条（与快照一致）");
    }

    // ---------- ⑰-c（P5 ③）提交审批后审批人待办里出现该调拨单 ----------

    #[tokio::test]
    async fn submit_creates_pending_todo_for_super_admin() {
        let Some(_guard) = prepare().await else {
            return;
        };
        ensure_seed_flow().await;
        let (id, row) = transfer_in_status(STATUS_PENDING).await;
        let instance_id = row.approval_instance_id.expect("② 提交审批必须生成实例");

        let todos = db()
            .find_with_params(
                "SELECT t.status, t.assignee_id, t.assignee_name, t.assignee_source, t.node_key \
                   FROM biz_flow_task t WHERE t.instance_id = ?::uuid",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查审批待办");
        assert_eq!(
            todos.len(),
            1,
            "③ 单节点审批：待办恰好 1 条（不给同一人重复派单）"
        );
        let todo = &todos[0];
        assert_eq!(
            todo.get_typed::<String>("status").expect("status"),
            "PENDING"
        );
        assert_eq!(
            todo.get_typed::<String>("node_key").expect("node"),
            "n_approve"
        );
        assert_eq!(
            todo.get_typed::<String>("assignee_source").expect("source"),
            "ADMIN",
            "③ 审批人来源必须是种子节点的 ADMIN（超管展开）"
        );
        let assignee: Uuid = todo.get_typed("assignee_id").expect("assignee_id");
        // 待办必须落在真实存在的超管账号上（否则审批中心里没人看得到）
        let is_super: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM sys_user u \
                   JOIN sys_user_role ur ON ur.user_id = u.id \
                   JOIN sys_role r ON r.id = ur.role_id \
                  WHERE u.id = ?::uuid AND r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL",
                vec![Value::from(&assignee)],
            )
            .await
            .expect("查超管角色");
        assert_eq!(is_super, 1, "③ 待办审批人必须是 SUPER_ADMIN 角色成员");

        // ③ 按调拨单反查也能命中（审批中心「待办」按 biz_type + biz_id 过滤的口径）
        let hit: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_instance i \
                   JOIN biz_flow_task t ON t.instance_id = i.id \
                  WHERE i.biz_type = 'STOCK_TRANSFER' AND i.biz_id = ?::uuid \
                    AND t.status = 'PENDING'",
                vec![Value::from(&id)],
            )
            .await
            .expect("按单据反查待办");
        assert_eq!(hit, 1, "③ 审批待办里能按调拨单找到这条待办");
    }

    // ---------- ⑰-d（P5 ⑦）审批中不许人工直推 APPROVED（409 + 65030） ----------

    #[tokio::test]
    async fn manual_approve_blocked_while_approval_pending() {
        let Some(_guard) = prepare().await else {
            return;
        };
        ensure_seed_flow().await;
        let applicant = uuid(U_APPLICANT);
        let (id, row) = transfer_in_status(STATUS_PENDING).await;
        let instance_id = row.approval_instance_id.expect("实例 id");

        let err = change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
            .await
            .expect_err("⑦ 审批还在跑时，人工直推 APPROVED 必须被拒");
        assert_eq!(err.status, 409, "⑦ HTTP 409（不是 500/400）");
        assert_eq!(
            err.code,
            error_code::TRANSFER_FLOW_GATE_BLOCKED,
            "⑦ 业务码 65030"
        );
        assert_eq!(err.code, 65030);
        assert!(
            err.msg.contains("审批中"),
            "⑦ 文案要说清「在审批中」：{}",
            err.msg
        );

        // 单据与审批单都原地不动
        assert_eq!(refresh(id).await.status, STATUS_PENDING);
        let instance = flow_model::find_instance(instance_id)
            .await
            .expect("查实例")
            .expect("实例存在");
        assert_eq!(instance.status, "PENDING", "⑦ 被拦后审批单仍是 PENDING");

        // 撤回仍放行（正规出口），撤回会终止审批单
        change_transfer_status(id, &status_req(STATUS_CANCELLED, Some("不调了")), applicant)
            .await
            .expect("⑦ 门闸只拦 APPROVED，撤回必须放行");
        assert_eq!(refresh(id).await.status, STATUS_CANCELLED);
    }

    // ---------- ⑰-e（P5 ⑥）没配流程时：提交成功但无实例，允许人工推 APPROVED ----------

    #[tokio::test]
    async fn submit_without_flow_degrades_to_manual_approve() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let (def_id, _) = ensure_seed_flow().await;
        // 模拟「客户没配流程」：软删种子定义（降级路径的唯一判据是「查不到已发布流程」）
        exec(
            "UPDATE biz_flow_def SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&def_id)],
            "软删调拨流程（模拟未配置）",
        )
        .await;
        assert!(
            transfer_model::published_transfer_flow()
                .await
                .expect("查流程")
                .is_none(),
            "⑥ 前置：软删后必须查不到已发布流程"
        );

        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(&body(vec![item(M_PLAIN, "2")]), Some(applicant), None)
            .await
            .expect("建单");
        change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
            .await
            .expect("⑥ 没配流程时提交审批必须照样成功（降级，不是 500）");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_PENDING);
        assert!(
            row.approval_instance_id.is_none(),
            "⑥ 降级路径：没有实例可挂，approval_instance_id 必须为空"
        );

        // 降级路径下人工推 APPROVED 必须放行（否则单据彻底卡死）
        change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
            .await
            .expect("⑥ 没审批单时人工推 APPROVED 必须放行");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_APPROVED);
        assert!(row.approved_at.is_some(), "⑥ 人工批准要写 approved_at");

        // 复原（prepare 里也会自愈，这里显式恢复以免影响同批次后续断言）
        exec(
            "UPDATE biz_flow_def SET deleted_at = NULL WHERE id = ?::uuid",
            vec![Value::from(&def_id)],
            "恢复调拨流程",
        )
        .await;
    }

    // ---------- ⑰-f（P5 ⑨）撤回（PENDING → CANCELLED）终止在跑的审批单 ----------

    #[tokio::test]
    async fn withdraw_cancels_running_approval_instance() {
        let Some(_guard) = prepare().await else {
            return;
        };
        ensure_seed_flow().await;
        let applicant = uuid(U_APPLICANT);
        let (id, row) = transfer_in_status(STATUS_PENDING).await;
        let instance_id = row.approval_instance_id.expect("实例 id");

        change_transfer_status(
            id,
            &status_req(STATUS_CANCELLED, Some("仓库临时不调了")),
            applicant,
        )
        .await
        .expect("⑨ 撤回必须成功");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_CANCELLED);
        assert!(row.cancelled_at.is_some());

        let instance = flow_model::find_instance(instance_id)
            .await
            .expect("查实例")
            .expect("实例存在");
        assert_eq!(
            instance.status, "CANCELLED",
            "⑨ 撤回必须把在跑审批单一起终止（否则 uk_biz_flow_instance_biz_active 会让重新提交永久 409）"
        );
        assert!(
            instance.ended_at.is_some(),
            "⑨ 终止的实例必须有 ended_at（ck_biz_flow_instance_end）"
        );
        // 待办也不能留 PENDING（脏待办会污染审批中心的待办数与统计）
        let pending: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查待办");
        assert_eq!(pending, 0, "⑨ 撤回后不许留 PENDING 待办");
    }

    // ---------- ⑰-g（P5 ⑧ + ⑩）审批通过 → 发货 → 收货：链路通 + W3c 库存双写不变 ----------

    #[tokio::test]
    async fn approved_transfer_continues_to_done_and_settles_inventory() {
        let Some(_guard) = prepare().await else {
            return;
        };
        ensure_seed_flow().await;
        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(&body(vec![item(M_PLAIN, "5")]), Some(applicant), None)
            .await
            .expect("建单");
        change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
            .await
            .expect("提交审批");
        let instance_id = refresh(id).await.approval_instance_id.expect("实例 id");

        // 审批通过：走**生产同款路径** —— 引擎在事务里投票落终态 → 再回调调拨单
        approve_instance(instance_id).await;
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_APPROVED, "⑧ 审批通过 → 调拨单 APPROVED");
        assert!(row.approved_at.is_some(), "⑧ approved_at 有值");

        // ⑧ 通过后能继续流转：APPROVED → IN_TRANSIT → DONE
        change_transfer_status(id, &status_req(STATUS_IN_TRANSIT, None), applicant)
            .await
            .expect("⑧ 审批通过后必须能发货");
        assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
        change_transfer_status(id, &status_req(STATUS_DONE, None), applicant)
            .await
            .expect("⑧ 在途必须能收货");
        let row = refresh(id).await;
        assert_eq!(row.status, STATUS_DONE);
        assert!(row.received_at.is_some());

        // ⑩ W3c 的库存双写没被本卡破坏：调出仓 100-5=95、调入仓 10+5=15，两条流水
        assert_dec_eq(
            inventory_qty(M_PLAIN, WH_FROM).await,
            "95",
            "⑩ 调出仓扣减 5",
        );
        assert_dec_eq(inventory_qty(M_PLAIN, WH_TO).await, "15", "⑩ 调入仓增加 5");
        assert_dec_eq(
            inventory_amount(M_PLAIN, WH_FROM).await,
            "1187.5",
            "⑩ 调出仓金额 1250 - 5×12.5",
        );
        assert_dec_eq(
            inventory_amount(M_PLAIN, WH_TO).await,
            "142.5",
            "⑩ 调入仓金额 80 + 5×12.5（成本原价转移）",
        );
        let txns = db()
            .find_with_params(
                "SELECT txn_type, qty::text AS qty FROM biz_inventory_txn \
                  WHERE doc_type = 'STOCK_TRANSFER' AND doc_id = ?::uuid ORDER BY txn_type",
                vec![Value::from(&id)],
            )
            .await
            .expect("查库存流水");
        assert_eq!(txns.len(), 2, "⑩ 一张调拨单恰好两条流水（OUT + IN）");
        let kinds: Vec<String> = txns
            .iter()
            .map(|row| row.get_typed::<String>("txn_type").expect("txn_type"))
            .collect();
        assert_eq!(kinds, vec!["IN".to_string(), "OUT".to_string()]);
        for txn in &txns {
            let qty: String = txn.get_typed("qty").expect("qty");
            assert_eq!(qty, "5.0000", "⑩ 流水数量恒正（ck_biz_inv_txn_qty 禁负数）");
        }
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

    // ==================== W3c：IN_TRANSIT → DONE 的库存双写 + 成本原价转移 ====================
    //
    // 夹具基线（[`prepare`] 每个用例都重置）：
    //
    // ```text
    // M_PLAIN（按数量）  分仓 100 件 @ 12.5 = 1250        总仓 10 件 @ 8 = 80
    // M_SN  （一物一码）分仓 2 件 @ 30 = 60（SERIAL_1/2 都在分仓 IN_STOCK）
    // ```

    /// 夹具数字（`Decimal` 的 `FromStr` 在测试里读起来比 `Decimal::new(5, 0)` 清楚）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("夹具数字")
    }

    async fn query(sql: &str, params: Vec<Value>) -> Vec<Row> {
        db().find_with_params(sql, params)
            .await
            .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {sql}"))
    }

    /// 某物料在某仓**整仓汇总桶**的账面数量（行不存在 → `None`）。
    async fn bucket_qty(material: &str, warehouse: &str) -> Option<Decimal> {
        let rows = query(
            "SELECT qty::numeric(18,4) AS qty FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
        )
        .await;
        rows.first()
            .map(|row| row.get_typed::<Decimal>("qty").expect("qty"))
    }

    async fn bucket_amount(material: &str, warehouse: &str) -> Option<Decimal> {
        let rows = query(
            "SELECT amount::numeric(18,4) AS amount FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
        )
        .await;
        rows.first()
            .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
    }

    async fn bucket_avg_cost(material: &str, warehouse: &str) -> Option<Decimal> {
        let rows = query(
            "SELECT avg_cost::numeric(18,6) AS avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
        )
        .await;
        rows.first()
            .map(|row| row.get_typed::<Decimal>("avg_cost").expect("avg_cost"))
    }

    async fn needed(material: &str, warehouse: &str) -> Decimal {
        bucket_qty(material, warehouse)
            .await
            .unwrap_or_else(|| panic!("{material} @ {warehouse} 的库存行必须存在"))
    }

    /// 某张单的库存流水（按方向排序，便于逐条断言）。
    async fn txns_of(transfer_id: Uuid) -> Vec<Row> {
        query(
            "SELECT txn_type, biz_type, doc_type, doc_no, line_no, \
                    qty::numeric(18,4) AS qty, unit_cost::numeric(18,6) AS unit_cost, \
                    amount::numeric(18,4) AS amount, \
                    before_qty::numeric(18,4) AS before_qty, after_qty::numeric(18,4) AS after_qty, \
                    before_amount::numeric(18,4) AS before_amount, \
                    after_amount::numeric(18,4) AS after_amount, warehouse_id \
               FROM biz_inventory_txn \
              WHERE doc_type = 'STOCK_TRANSFER' AND doc_id = ?::uuid \
              ORDER BY txn_type, line_no",
            vec![Value::from(&transfer_id)],
        )
        .await
    }

    /// 文本列（`txn_type` / `biz_type` / `doc_no` …）。
    fn row_text(row: &Row, column: &str) -> String {
        row.get_typed::<String>(column)
            .unwrap_or_else(|err| panic!("列 {column} 不是文本：{err}"))
    }

    /// 数值列取字符串（断言里逐字比对 `"5.0000"` 比 `Decimal` 相等更能钉住标度）。
    fn txn_text(row: &Row, column: &str) -> String {
        row.get_typed::<Decimal>(column)
            .unwrap_or_else(|err| panic!("列 {column} 不是数字：{err}"))
            .to_string()
    }

    async fn serial_warehouse(serial_id: &str) -> Option<Uuid> {
        let rows = query(
            "SELECT warehouse_id FROM biz_material_serial WHERE id = ?::uuid",
            vec![Value::from(&uuid(serial_id))],
        )
        .await;
        rows.first()
            .map(|row| row.get_typed("warehouse_id").expect("warehouse_id"))
    }

    async fn serial_in_stock_count(material: &str, warehouse: &str) -> i64 {
        let rows = query(
            "SELECT COUNT(*) AS cnt FROM biz_material_serial \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND status = 'IN_STOCK' AND deleted_at IS NULL",
            vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
        )
        .await;
        rows.first()
            .map(|row| row.get_typed::<i64>("cnt").expect("cnt"))
            .unwrap_or(0)
    }

    /// 建单 → PENDING →（真的走审批）→ APPROVED → IN_TRANSIT（结算前的公共前置）。
    async fn create_and_ship(items: Vec<TransferItemReq>) -> Uuid {
        let applicant = uuid(U_APPLICANT);
        let id = create_transfer(&body(items), Some(applicant), Some("w3b_applicant"))
            .await
            .expect("建单必须成功");
        change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
            .await
            .unwrap_or_else(|err| panic!("推进到 {STATUS_PENDING} 失败：{}", err.msg));
        // PENDING → APPROVED 有人工门闸（65030）：挂了审批单就必须走审批主线
        match transfer_model::find_by_id(id)
            .await
            .expect("查调拨单")
            .and_then(|row| row.approval_instance_id)
        {
            Some(instance_id) => super::approve_flow_instance(instance_id).await,
            None => {
                change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
                    .await
                    .unwrap_or_else(|err| panic!("推进到 {STATUS_APPROVED} 失败：{}", err.msg));
            }
        }
        change_transfer_status(id, &status_req(STATUS_IN_TRANSIT, None), applicant)
            .await
            .unwrap_or_else(|err| panic!("推进到 {STATUS_IN_TRANSIT} 失败：{}", err.msg));
        id
    }

    /// 收货（= 触发 W3c 的库存双写）。
    async fn receive(id: Uuid) -> std::result::Result<(), ApiError> {
        change_transfer_status(id, &status_req(STATUS_DONE, None), uuid(U_APPLICANT)).await
    }

    async fn receive_ok(id: Uuid) {
        receive(id)
            .await
            .unwrap_or_else(|err| panic!("收货结算必须成功：{} {}", err.code, err.msg));
    }

    // ---------- W3c ① 数量双写：调出仓 -、调入仓 + ----------

    #[tokio::test]
    async fn w3c_settle_moves_qty_between_warehouses() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let from_before = needed(M_PLAIN, WH_FROM).await;
        let to_before = needed(M_PLAIN, WH_TO).await;
        receive_ok(id).await;
        assert_eq!(
            needed(M_PLAIN, WH_FROM).await,
            from_before - dec("5"),
            "调出仓必须 -5"
        );
        assert_eq!(
            needed(M_PLAIN, WH_TO).await,
            to_before + dec("5"),
            "调入仓必须 +5"
        );
        assert_eq!(refresh(id).await.status, STATUS_DONE);
    }

    // ---------- W3c ② 序列号跟着走 ----------

    #[tokio::test]
    async fn w3c_settle_moves_serials_to_target_warehouse() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let mut line = item(M_SN, "1");
        line.serial_ids = vec![SERIAL_1.to_string()];
        let id = create_and_ship(vec![line]).await;
        assert_eq!(
            serial_warehouse(SERIAL_1).await,
            Some(uuid(WH_FROM)),
            "结算前序列号在调出仓"
        );
        receive_ok(id).await;
        assert_eq!(
            serial_warehouse(SERIAL_1).await,
            Some(uuid(WH_TO)),
            "结算后序列号必须到调入仓"
        );
        assert_eq!(
            serial_warehouse(SERIAL_2).await,
            Some(uuid(WH_FROM)),
            "没上单的序列号不得被顺带挪走"
        );
    }

    // ---------- W3c ③ 调出流水：OUT / TRANSFER_OUT ----------

    #[tokio::test]
    async fn w3c_settle_writes_out_txn_row() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let no = refresh(id).await.transfer_no;
        receive_ok(id).await;
        let txns = txns_of(id).await;
        let out: Vec<&Row> = txns
            .iter()
            .filter(|row| row_text(row, "txn_type") == "OUT")
            .collect();
        assert_eq!(out.len(), 1, "调出流水必须正好 1 条");
        let row = out[0];
        assert_eq!(row_text(row, "biz_type"), "TRANSFER_OUT");
        assert_eq!(row_text(row, "doc_type"), "STOCK_TRANSFER");
        assert_eq!(row_text(row, "doc_no"), no, "流水必须挂单号");
        assert_eq!(
            row.get_typed::<Uuid>("warehouse_id").expect("仓"),
            uuid(WH_FROM),
            "调出流水挂在调出仓"
        );
        assert_eq!(txn_text(row, "qty"), "5.0000", "qty 恒正（禁负数）");
        assert_eq!(
            txn_text(row, "unit_cost"),
            "12.500000",
            "原价 = 调出仓均价快照"
        );
        assert_eq!(txn_text(row, "amount"), "62.5000");
    }

    // ---------- W3c ④ 调入流水：IN / TRANSFER_IN ----------

    #[tokio::test]
    async fn w3c_settle_writes_in_txn_row() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        receive_ok(id).await;
        let txns = txns_of(id).await;
        let r#in: Vec<&Row> = txns
            .iter()
            .filter(|row| row_text(row, "txn_type") == "IN")
            .collect();
        assert_eq!(r#in.len(), 1, "调入流水必须正好 1 条");
        let row = r#in[0];
        assert_eq!(row_text(row, "biz_type"), "TRANSFER_IN");
        assert_eq!(
            row.get_typed::<Uuid>("warehouse_id").expect("仓"),
            uuid(WH_TO),
            "调入流水挂在调入仓"
        );
        assert_eq!(txn_text(row, "qty"), "5.0000");
    }

    // ---------- W3c ⑤ 成本守恒：调出 amount 减少 == 调入 amount 增加 ----------

    #[tokio::test]
    async fn w3c_settle_keeps_cost_conserved() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let from_before = bucket_amount(M_PLAIN, WH_FROM).await.expect("调出仓金额");
        let to_before = bucket_amount(M_PLAIN, WH_TO).await.expect("调入仓金额");
        receive_ok(id).await;
        let from_after = bucket_amount(M_PLAIN, WH_FROM).await.expect("调出仓金额");
        let to_after = bucket_amount(M_PLAIN, WH_TO).await.expect("调入仓金额");
        let out = from_before - from_after;
        let r#in = to_after - to_before;
        assert_eq!(out, dec("62.5"), "调出仓金额减少 = 5 × 12.5");
        assert_eq!(r#in, out, "金额不能凭空产生或消失");

        // 流水侧同样守恒（两笔流水金额相等）
        let txns = txns_of(id).await;
        let out_amt: Decimal = txns
            .iter()
            .filter(|row| row_text(row, "txn_type") == "OUT")
            .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
            .sum();
        let in_amt: Decimal = txns
            .iter()
            .filter(|row| row_text(row, "txn_type") == "IN")
            .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
            .sum();
        assert_eq!(out_amt, in_amt, "流水 OUT 合计 == IN 合计");
        assert_eq!(out_amt, dec("62.5"));
    }

    // ---------- W3c ⑥ 调入仓移动加权重算 avg_cost ----------

    #[tokio::test]
    async fn w3c_settle_recomputes_target_avg_cost() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let old_qty = needed(M_PLAIN, WH_TO).await;
        let old_amount = bucket_amount(M_PLAIN, WH_TO).await.expect("金额");
        receive_ok(id).await;
        let new_qty = needed(M_PLAIN, WH_TO).await;
        let new_amount = bucket_amount(M_PLAIN, WH_TO).await.expect("金额");
        let new_cost = bucket_avg_cost(M_PLAIN, WH_TO).await.expect("均价");
        assert_eq!(new_qty, old_qty + dec("5"));
        assert_eq!(new_amount, old_amount + dec("62.5"));
        // 10 件 @8 = 80，再收 5 件 @12.5 = 62.5 → 142.5 / 15 = 9.5
        assert_eq!(new_cost, dec("9.5"), "新均价 = 新金额 / 新数量");
        // 调出仓的均价不因出库而改变（移动加权口径）
        assert_eq!(
            bucket_avg_cost(M_PLAIN, WH_FROM).await.expect("调出仓均价"),
            dec("12.5"),
            "出库不改调出仓均价"
        );
    }

    // ---------- W3c ⑦ 调入仓库存行不存在 → INSERT 建档 ----------

    #[tokio::test]
    async fn w3c_settle_creates_missing_target_row() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // M_SN 在总仓本来没有库存行（夹具只给分仓建了）
        assert!(
            bucket_qty(M_SN, WH_TO).await.is_none(),
            "前置：总仓没有 M_SN 的库存行"
        );
        let mut line = item(M_SN, "2");
        line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        let id = create_and_ship(vec![line]).await;
        receive_ok(id).await;
        assert_eq!(needed(M_SN, WH_TO).await, dec("2"), "建档后数量 = 调入量");
        assert_eq!(
            bucket_avg_cost(M_SN, WH_TO).await.expect("均价"),
            dec("30"),
            "建档行的均价 = 原价（不是 0）"
        );
        assert_eq!(
            bucket_amount(M_SN, WH_TO).await.expect("金额"),
            dec("60"),
            "建档金额 = 数量 × 原价"
        );
    }

    // ---------- W3c ⑧ 串行多明细（3 个物料） ----------

    #[tokio::test]
    async fn w3c_settle_handles_three_lines_in_one_doc() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 第三个物料（再插一个按数量的物料 L3）一起走
        exec(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                      safety_stock, costing_method, tax_rate, status, serial_required) \
             VALUES ('d3b0000c-0000-4000-8000-000000000003', 'W3C-M-L3', 'W3C 第三物料', \
                     ?::uuid, 'RAW', 0, 'MOVING_AVG', 0, 1, false) ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(UNIT))],
            "第三物料",
        )
        .await;
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                        avg_cost, amount) \
             VALUES (gen_random_uuid(), 'd3b0000c-0000-4000-8000-000000000003', ?::uuid, NULL, \
                     6, 0, 2, 12) ON CONFLICT DO NOTHING",
            vec![Value::from(&uuid(WH_FROM))],
            "第三物料库存",
        )
        .await;
        let mut sn = item(M_SN, "1");
        sn.serial_ids = vec![SERIAL_1.to_string()];
        let id = create_and_ship(vec![
            item(M_PLAIN, "5"),
            sn,
            item("d3b0000c-0000-4000-8000-000000000003", "4"),
        ])
        .await;
        receive_ok(id).await;

        assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("95"));
        assert_eq!(needed(M_SN, WH_FROM).await, dec("1"));
        assert_eq!(
            bucket_qty("d3b0000c-0000-4000-8000-000000000003", WH_FROM)
                .await
                .expect("第三物料库存行"),
            dec("2")
        );
        assert_eq!(needed(M_PLAIN, WH_TO).await, dec("15"));
        assert_eq!(needed(M_SN, WH_TO).await, dec("1"));
        assert_eq!(
            bucket_qty("d3b0000c-0000-4000-8000-000000000003", WH_TO)
                .await
                .expect("第三物料在总仓建档"),
            dec("4")
        );
        // 6 条流水（3 行 × 进出各一条）
        assert_eq!(txns_of(id).await.len(), 6, "三行必须各写进/出两条流水");
        let detail = detail_of(id).await.expect("详情");
        assert!(
            detail
                .items
                .iter()
                .all(|item| item.received_qty == item.qty),
            "三行都必须回填 received_qty"
        );
    }

    // ---------- W3c ⑨ 并发：两笔各扣 5、库存 8 → 一个成功一个 409 ----------

    #[tokio::test]
    async fn w3c_settle_concurrent_transfers_only_one_wins() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 把分仓库存压到 8（两笔各 5 只能成一笔）
        exec(
            "UPDATE biz_inventory SET qty = 8, amount = 8 * avg_cost \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
            "压库存到 8",
        )
        .await;
        let a = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let b = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let (ra, rb) = tokio::join!(receive(a), receive(b));
        let mut statuses = Vec::new();
        for (id, result) in [(a, ra), (b, rb)] {
            match result {
                Ok(()) => statuses.push((id, 200)),
                Err(err) => statuses.push((id, err.status)),
            }
        }
        statuses.sort_by_key(|(_, status)| *status);
        assert_eq!(
            statuses
                .iter()
                .map(|(_, status)| *status)
                .collect::<Vec<_>>(),
            vec![200, 409],
            "库存 8、两笔各扣 5：必须一个成功一个 409，实际 {statuses:?}"
        );
        assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("3"), "只能扣掉一笔");
        // 成功那张单是 DONE，失败那张单仍在 IN_TRANSIT（回滚）
        let done = statuses
            .iter()
            .find(|(_, s)| *s == 200)
            .expect("成功的那张")
            .0;
        let lost = statuses
            .iter()
            .find(|(_, s)| *s == 409)
            .expect("失败的那张")
            .0;
        assert_eq!(refresh(done).await.status, STATUS_DONE);
        assert_eq!(refresh(lost).await.status, STATUS_IN_TRANSIT);
    }

    // ---------- W3c ⑩ 库存不足 → 409 + 状态与库存都不动（回滚验证） ----------

    #[tokio::test]
    async fn w3c_settle_insufficient_stock_rolls_back_to_in_transit() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        // 发货后库存被别人扣走（模拟并发 / 手工改库）
        exec(
            "UPDATE biz_inventory SET qty = 1, amount = 1 * avg_cost \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
            "压库存到 1",
        )
        .await;
        let err = receive(id).await.expect_err("库存 1 调 5 必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("库存不足"), "{}", err.msg);
        // 状态回滚
        assert_eq!(
            refresh(id).await.status,
            STATUS_IN_TRANSIT,
            "结算失败后必须仍是 IN_TRANSIT"
        );
        // 库存一行都没动、流水一条都没写
        assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("1"), "调出仓不得被扣");
        assert_eq!(needed(M_PLAIN, WH_TO).await, dec("10"), "调入仓不得增加");
        assert!(txns_of(id).await.is_empty(), "失败必须没有流水");
        let detail = detail_of(id).await.expect("详情");
        assert_eq!(detail.items[0].received_qty, Decimal::ZERO, "明细不得回填");
    }

    // ---------- W3c ⑪ 一物一码：qty == COUNT(serial IN_STOCK) ----------

    #[tokio::test]
    async fn w3c_settle_keeps_serial_invariant() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let mut line = item(M_SN, "2");
        line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        let id = create_and_ship(vec![line]).await;
        receive_ok(id).await;
        for warehouse in [WH_FROM, WH_TO] {
            let book = needed(M_SN, warehouse).await;
            let serials = serial_in_stock_count(M_SN, warehouse).await;
            assert_eq!(
                book,
                Decimal::from(serials),
                "不变式③：{warehouse} 的账面数量必须等于在库序列号件数"
            );
        }
        assert_eq!(needed(M_SN, WH_FROM).await, Decimal::ZERO);
        assert_eq!(needed(M_SN, WH_TO).await, dec("2"));
    }

    // ---------- W3c ⑫ 按数量管理的物料：不动任何序列号 ----------

    #[tokio::test]
    async fn w3c_settle_does_not_touch_serials_of_quantity_material() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let before = serial_in_stock_count(M_SN, WH_FROM).await;
        let id = create_and_ship(vec![item(M_PLAIN, "3")]).await;
        receive_ok(id).await;
        assert_eq!(
            serial_in_stock_count(M_SN, WH_FROM).await,
            before,
            "数量物料调拨不得影响任何序列号"
        );
        assert_eq!(serial_warehouse(SERIAL_1).await, Some(uuid(WH_FROM)));
        assert_eq!(serial_warehouse(SERIAL_2).await, Some(uuid(WH_FROM)));
    }

    // ---------- W3c ⑬ 序列号个数 != 数量 → 400 ----------

    #[tokio::test]
    async fn w3c_settle_rejects_serial_count_mismatch() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let mut line = item(M_SN, "2");
        line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        let id = create_and_ship(vec![line]).await;
        // 建单后被改库：只留一条序列号关联（数量 2 vs 序列号 1）
        exec(
            "DELETE FROM biz_stock_transfer_item_serial \
              WHERE id IN (SELECT l.id FROM biz_stock_transfer_item_serial l \
                             JOIN biz_stock_transfer_item i ON i.id = l.item_id \
                            WHERE i.transfer_id = ?::uuid \
                            ORDER BY l.created_at DESC LIMIT 1)",
            vec![Value::from(&id)],
            "删一条序列号关联",
        )
        .await;
        let err = receive(id)
            .await
            .expect_err("序列号 1 个 vs 数量 2 必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("序列号"), "{}", err.msg);
        assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
        assert_eq!(
            needed(M_SN, WH_FROM).await,
            dec("2"),
            "校验失败不得先扣库存"
        );
    }

    // ---------- W3c ⑭ 幂等：结算两次不重复扣 ----------

    #[tokio::test]
    async fn w3c_settle_twice_is_idempotent() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        receive_ok(id).await;
        let row = refresh(id).await;
        let qty_after = needed(M_PLAIN, WH_FROM).await;
        let amount_after = bucket_amount(M_PLAIN, WH_FROM).await.expect("金额");
        // 直接再调一次模型层（模拟运维重放 / 回调重复）
        let outcome = transfer_model::settle_transfer(
            db(),
            row.id,
            &row.transfer_no,
            row.from_warehouse_id,
            row.to_warehouse_id,
            Some(uuid(U_APPLICANT)),
            Utc::now(),
        )
        .await
        .expect("重复调用必须安全返回（幂等）而不是报错");
        assert!(!outcome.settled, "第二次必须是幂等跳过");
        assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_after, "不重复扣");
        assert_eq!(
            bucket_amount(M_PLAIN, WH_FROM).await.expect("金额"),
            amount_after,
            "不重复扣金额"
        );
        assert_eq!(txns_of(id).await.len(), 2, "流水仍然只有 2 条");
    }

    // ---------- W3c ⑮ 终态与非法来源：DONE 之后 / 其它状态 → 409 ----------

    #[tokio::test]
    async fn w3c_done_is_sticky_and_other_states_rejected() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "1")]).await;
        receive_ok(id).await;
        let qty_after = needed(M_PLAIN, WH_FROM).await;
        // DONE → DONE：非法流转（幂等保护在接口层）
        let err = receive(id).await.expect_err("DONE 之后再调必须 409");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("非法状态流转"), "{}", err.msg);
        assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_after, "不得二次扣");

        // 取消单不能收货
        let cancelled = create_transfer(
            &body(vec![item(M_PLAIN, "1")]),
            Some(uuid(U_APPLICANT)),
            None,
        )
        .await
        .expect("建单");
        change_transfer_status(
            cancelled,
            &status_req(STATUS_CANCELLED, Some("不调了")),
            uuid(U_APPLICANT),
        )
        .await
        .expect("DRAFT 能直接取消");
        let err = receive(cancelled).await.expect_err("CANCELLED 不能收货");
        assert_eq!(err.status, 409);

        // 草稿不能跳级收货
        let draft = create_transfer(
            &body(vec![item(M_PLAIN, "1")]),
            Some(uuid(U_APPLICANT)),
            None,
        )
        .await
        .expect("建单");
        let err = receive(draft).await.expect_err("DRAFT 不能收货");
        assert_eq!(err.status, 409);
    }

    // ---------- W3c ⑯ 序列号已不在调出仓 → 409 且整笔回滚 ----------

    #[tokio::test]
    async fn w3c_settle_rejects_serial_outside_source() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let mut line = item(M_SN, "2");
        line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
        let id = create_and_ship(vec![line]).await;
        // 发货后有人把 SERIAL_2 挪去总仓（另一张单据出库 / 手工改库）
        exec(
            "UPDATE biz_material_serial SET warehouse_id = ?::uuid WHERE id = ?::uuid",
            vec![Value::from(&uuid(WH_TO)), Value::from(&uuid(SERIAL_2))],
            "挪走序列号",
        )
        .await;
        let err = receive(id).await.expect_err("序列号不全在调出仓必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("序列号"), "{}", err.msg);
        // 整笔回滚：库存、SERIAL_1 都回到原样
        assert_eq!(needed(M_SN, WH_FROM).await, dec("2"));
        assert_eq!(
            serial_warehouse(SERIAL_1).await,
            Some(uuid(WH_FROM)),
            "先转移成功的序列号也必须回滚"
        );
        assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
        assert!(txns_of(id).await.is_empty());
    }

    // ---------- W3c ⑰ 明细回填：shipped/received/unit_cost/amount ----------

    #[tokio::test]
    async fn w3c_settle_fills_item_shipped_received_and_cost() {
        let Some(_guard) = prepare().await else {
            return;
        };
        let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
        let before = transfer_model::items_of(id).await.expect("明细");
        assert_eq!(
            before[0].shipped_qty,
            Decimal::ZERO,
            "IN_TRANSIT 时还没回填"
        );
        receive_ok(id).await;
        let items = transfer_model::items_of(id).await.expect("明细");
        assert_eq!(items[0].shipped_qty, dec("5"), "shipped_qty = qty");
        assert_eq!(items[0].received_qty, dec("5"), "received_qty = qty");
        assert_eq!(items[0].unit_cost, dec("12.5"), "unit_cost = 原价快照");
        assert_eq!(items[0].amount, dec("62.5"), "amount = 数量 × 原价");
        assert_eq!(
            refresh(id).await.total_amount,
            dec("62.5"),
            "单据头金额合计"
        );
        // 接口视图（详情）也要能看到已发/已收
        let detail = detail_of(id).await.expect("详情");
        assert_eq!(detail.items[0].shipped_qty, dec("5"));
        assert_eq!(detail.items[0].received_qty, dec("5"));
    }

    // ---------- W3c ⑱ 库位维度：没有汇总桶的仓按库位行记账 ----------

    #[tokio::test]
    async fn w3c_settle_uses_location_row_when_no_bucket() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 把 M_PLAIN 的整仓汇总桶换成两个库位行（模拟纯库位记账的仓）
        exec(
            "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
            vec![
                Value::from(&uuid(M_PLAIN)),
                array_param(&[uuid(WH_FROM), uuid(WH_TO)]),
            ],
            "清掉汇总桶",
        )
        .await;
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                        avg_cost, amount) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 7, 0, 4, 28)",
            vec![
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_FROM)),
                Value::from(&uuid(LOC_FROM)),
            ],
            "库位库存行",
        )
        .await;
        let mut line = item(M_PLAIN, "3");
        line.from_location_id = Some(LOC_FROM.to_string());
        line.to_location_id = Some(LOC_TO.to_string());
        let id = create_and_ship(vec![line]).await;
        receive_ok(id).await;
        // 调出：库位行 7 → 4（金额 28 → 16，原价 4）
        let from_row = query(
            "SELECT qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount \
               FROM biz_inventory WHERE location_id = ?::uuid",
            vec![Value::from(&uuid(LOC_FROM))],
        )
        .await;
        assert_eq!(
            from_row[0].get_typed::<Decimal>("qty").expect("qty"),
            dec("4")
        );
        assert_eq!(
            from_row[0].get_typed::<Decimal>("amount").expect("amount"),
            dec("16")
        );
        // 调入：LOC_TO 建档 qty 3 / 均价 4 / 金额 12
        let to_row = query(
            "SELECT qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount, \
                    avg_cost::numeric(18,6) AS avg_cost \
               FROM biz_inventory WHERE location_id = ?::uuid",
            vec![Value::from(&uuid(LOC_TO))],
        )
        .await;
        assert_eq!(
            to_row[0].get_typed::<Decimal>("qty").expect("qty"),
            dec("3")
        );
        assert_eq!(
            to_row[0].get_typed::<Decimal>("amount").expect("amount"),
            dec("12")
        );
        assert_eq!(
            to_row[0]
                .get_typed::<Decimal>("avg_cost")
                .expect("avg_cost"),
            dec("4")
        );
    }

    // ---------- W3c ⑲ 结算金额与流水金额一致（ROUND 口径） ----------

    #[tokio::test]
    async fn w3c_settle_amount_rounding_matches_txn() {
        let Some(_guard) = prepare().await else {
            return;
        };
        // 均价改成 3.333333（6 位小数）× 3 件 = 9.999999 → 必须四舍五入成 10.0000 而不是各算一套
        exec(
            "UPDATE biz_inventory SET avg_cost = 3.333333, amount = qty * 3.333333 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
            "改均价",
        )
        .await;
        let id = create_and_ship(vec![item(M_PLAIN, "3")]).await;
        receive_ok(id).await;
        let txns = txns_of(id).await;
        let amounts: Vec<String> = txns.iter().map(|row| txn_text(row, "amount")).collect();
        assert_eq!(
            amounts,
            vec!["10.0000".to_string(), "10.0000".to_string()],
            "两条流水金额必须逐字相同（同一个 ROUND 结果）"
        );
        assert_eq!(refresh(id).await.total_amount, dec("10"));
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
