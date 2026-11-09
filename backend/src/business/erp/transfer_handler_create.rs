// ============================================================================
// transfer_handler_create.rs —— 建单 / 复制 / 改单：取号 + 明细落库 + 草稿态校验（建单口径，纯搬迁）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::transfer_handler_common::*;
use super::transfer_handler_query::*;
use crate::business::erp::transfer_dto::{CreateTransferReq, TransferDetailRes};
use crate::business::erp::transfer_model::{self, NewItem, NewTransfer};
use crate::shared::error_code;
use crate::utils;
use axum::Json;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

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
        reason: req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty()),
        remark: req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()),
        applicant_id: operator,
        applicant_name: applicant_name.map(str::trim).filter(|text| !text.is_empty()),
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

// ==================== ③b 复制为新单（D9） ====================

/// 复制接口的权限码 = **建单权限码**（卡面硬红线 5）。
pub const COPY_PERMISSION: &str = "business:erp:transfer:create";

/// 「复制为新单」的核心逻辑（endpoint 与单测共用）。
///
/// # 语义（卡面 §二）
///
/// ```text
/// 保留：调出仓 / 调入仓 / 明细（物料·数量·库位）/ 原因 / 备注
/// 清空：单号（重新走 biz_seq 取号）、id、created_* / updated_* / *_by、状态 → DRAFT、
///       审批实例 / 审批人 / 提交·通过·发货·收货时间 / 作废时间与原因、
///       明细行的已发·已收数量与单位成本/金额快照、明细行的序列号
/// 重算：申请人 = 当前操作人（这次是他在建单，不是原申请人）
/// 改：  调拨日期 = 今天（`transfer_date` 传 `None` → SQL 侧 `CURRENT_DATE`）
/// ```
///
/// # ★ 决策：序列号**不复制**
///
/// `biz_stock_transfer_item_serial` 记的是「这批实物号码跟着本单走」。
/// 原单的号码可能已在调入仓（`IN_STOCK`）或已报废，复制单再挂同一批号码：
/// ① 建单时的有效性校验只认「在调出仓 + 在库」，原单已结算的直接 400；
/// ② 即便侥幸通过，两张单会把同一批号码各调一次，库存与序列号必然对不上。
/// 所以复制单的序列号留空，由用户在提交前按实际货品重新选（一物一码物料届时必须选）。
///
/// # ★ 决策：调拨日期取「今天」
///
/// 同收货单：复制是新的调拨动作，沿用旧日期会在「按日期查列表 / 按期间统计」时
/// 与原单撞在一起，看不出哪张是后补的。
///
/// # 异常情况
///
/// - 403：无 `business:erp:transfer:create`；
/// - 404：原单不存在 / 已软删；
/// - 400：原单没有明细；
/// - 500：数据库异常。
pub async fn copy_transfer(id: Uuid, claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    crate::business::erp::period::ensure_permission(claims, COPY_PERMISSION, "复制调拨单")?;
    let operator = operator_of(claims);
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启复制事务"))?;

    // ① 读原单 + 明细（FOR SHARE，只读）
    let origin = match transfer_model::copy_source(&tx, id).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found("调拨单"));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读取待复制的调拨单"));
        }
    };
    let (origin, items) = origin;
    if items.is_empty() {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "调拨单 {} 没有任何明细，复制出来会是一张空单：请先给原单补明细，\
                 或直接新建一张并填写物料",
                origin.transfer_no
            ),
        ));
    }

    // ② 构造新单：新号 / DRAFT / 时间戳与序列号全清
    let lines: Vec<NewItem> = items
        .iter()
        .map(|item| NewItem {
            material_id: item.material_id,
            from_location_id: item.from_location_id,
            to_location_id: item.to_location_id,
            qty: item.qty,
            // 实物序列号不复制（见文档注释）
            serial_ids: Vec::new(),
        })
        .collect();
    let total_qty = lines
        .iter()
        .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, line| acc + line.qty);
    let new = NewTransfer {
        from_warehouse_id: origin.from_warehouse_id,
        to_warehouse_id: origin.to_warehouse_id,
        // `None` → SQL 侧 `CURRENT_DATE`（今天）
        transfer_date: None,
        reason: origin.reason.as_deref(),
        remark: origin.remark.as_deref(),
        applicant_id: operator,
        applicant_name: claims.username.as_deref(),
    };

    // ③ 插入新单 + 明细（`create` 内取号 + 写建单日志）
    match transfer_model::create(&tx, &new, &lines, total_qty, operator, now).await {
        Ok(new_id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交复制事务"))?;
            Ok(new_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "复制调拨单"))
        }
    }
}

/// 复制为新单（`POST /business/erp/transfer/{id}/copy`，权限 = 建单权限）。
#[nik::permission(
    path = "/business/erp/transfer/{id}/copy",
    method = "POST",
    permission = "business:erp:transfer:create"
)]
#[nik::post("/business/erp/transfer/{id}/copy")]
pub async fn transfer_copy(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TransferDetailRes> {
    let origin_id = parse_uuid(&id, "调拨单 id")?;
    let new_id = copy_transfer(origin_id, &claims).await?;
    Ok(Res::ok(detail_of(new_id).await?))
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
        reason: req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty()),
        remark: req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()),
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
