// ============================================================================
// stockin_handler_write.rs —— 写入：建单 / 复制 / 编辑 / 删除
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::{Extension, Json};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_common::*;
use super::stockin_handler_purchase::*;
use crate::business::erp::model;
use crate::business::erp::stockin_bridge;
use crate::business::erp::stockin_dto::{CreateStockInReq, StockInDetailRes};
use crate::business::erp::stockin_model::{
    self, NewStockIn, NewStockInLine, STATUS_DRAFT, STATUS_PENDING_CANCEL, STATUS_PENDING_POST,
    Totals, status_label,
};
use crate::business::shared::state_machine;
use crate::utils;

/// 建单核心逻辑（handler 与单测共用）。
///
/// 顺序：DTO 校验 + 算金额（纯函数）→ 供应商 / 仓库存在 → 来源采购单可收 →
/// （明细为空时按采购单未收数量带出）→ 逐行物料 / 库位 / 采购行校验 → 一个事务里「取号 + 单头 + 明细」。
pub async fn create_stock_in(
    req: &CreateStockInReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let mut request = req.clone();
    resolve_purchase_order(&mut request)?;
    let source_order_id =
        parse_optional_uuid(request.source_order_id.as_ref(), "purchase_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;

    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // ★ A4：供应商的唯一取值点。有源 → 从采购单派生（客户端填错即 400）；
    //   无源 → 必须显式 is_sourceless=true + 供应商 + 备注（AP-5）。
    let pointer = resolve_parent_pointer(
        source_order_id,
        request.is_sourceless,
        prepared.supplier_id_raw.as_deref(),
        prepared.remark.as_deref(),
        warehouse_id,
    )
    .await?;
    let supplier_id = pointer.supplier_id;
    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        is_sourceless: pointer.is_sourceless,
        in_date: prepared.in_date,
        invoice_received: prepared.invoice_received,
        remark: prepared.remark.as_deref(),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match stockin_model::create(&tx, &new, &lines, &totals, op, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建收货入库单"))
        }
    }
}

/// 新建收货入库单（`status = DRAFT`，单号 `IN-yyyyMMdd-000001`）。
#[nik::permission(
    path = "/business/erp/stock-in",
    method = "POST",
    permission = "business:erp:stock-in:create"
)]
#[nik::post("/business/erp/stock-in")]
pub async fn stock_in_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateStockInReq>,
) -> ResResult<StockInDetailRes> {
    let id = create_stock_in(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ①b 复制为新单（D9） ====================

/// 「复制为新单」的核心逻辑（endpoint 与单测共用，只跳过认证）。
///
/// # 语义（卡面 §二）
///
/// ```text
/// 保留：供应商 / 仓库 / 库位 / 明细（物料·数量·单价·税率）/ 发票标记 / 备注
/// 清空：单号（重新走 biz_seq 取号）、id、created_* / updated_* / *_by、
///       状态 → DRAFT、审批实例、凭证、应付、过账时间/人、作废时间/人/原因、
///       来源采购单（见下）与明细行的 source_item_id、明细行的序列号
/// 改：  入库日期 = 今天（CST）
/// ```
///
/// # ★ 决策：来源采购单**清空**（卡面 §二 的关键决策点，选 B）
///
/// 保留 `source_order_id` 看起来更「省事」，但原单**已经占用了那张采购单的待收额度**
/// （已过账收货单计入派生「已收数量」），复制单再挂同一个采购单，会二次占用同一批待收数量：
/// 轻则过账时超收被拒（409），重则两张单各收一次、待收数量算错且难查。
/// 所以新单一律 `source_order_id = NULL`，
/// 由用户在明细里重新关联（前端复制成功后会提示「请重新关联采购单」）。
///
/// # ★ 决策：入库日期取「今天」
///
/// 复制是**一次新的入库动作**，不是「补录原单那天」。沿用旧日期有两个坑：
/// ① 老单日期落在已结账期间 → 过账被期间闸拒（用户看到的是「复制出来的单不能过账」）；
/// ② 复制单与原单同日期同供应商同金额，月末对账时极易被当成重复入库。
///
/// # ★ 决策：明细行 `serial_ids` 不复制
///
/// 序列号是**实物一物一码**，原单那批号码已经跟着原单过账落库（`IN_STOCK`）。
/// 复制单再带同一批号码，要么过账时报「号码已被占用」，要么把同一件货记成两件。
/// 复制单的序列号按原路径处理：过大账时由系统取号或现场扫码提交。
///
/// # 异常情况
///
/// - 403：无 `business:erp:stock-in:create`；
/// - 404：原单不存在 / 已软删；
/// - 400：原单没有明细（`items 不能为空`）/ 原单缺供应商（库约束要求 `PURCHASE` 必填）；
/// - 409：取号器与库里的号段不一致（`write_error` 统一口径）；
/// - 500：数据库异常。
pub async fn copy_stock_in(id: Uuid, claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    // 行级权限闸：路由注解已拦一次，这里再判一次（单测能直接调业务函数证明 403）
    crate::business::erp::period::ensure_permission(claims, COPY_PERMISSION, "复制收货入库单")?;
    let op = state_machine::Operator::from(claims);
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启复制事务"))?;

    // ① 读原单 + 明细（FOR SHARE，只读；原单一个字节都不改）
    let origin = match stockin_model::copy_source(&tx, id).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found("收货入库单"));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读取待复制的收货入库单"));
        }
    };
    let (origin, items) = origin;
    if items.is_empty() {
        let _ = tx.rollback().await;
        return Err(bad_request(format!(
            "收货入库单 {} 没有任何明细，复制出来会是一张空单：请先给原单补明细，\
             或直接新建一张并填写物料",
            origin.in_no
        )));
    }
    let Some(supplier_id) = origin.supplier_id else {
        let _ = tx.rollback().await;
        return Err(bad_request(format!(
            "收货入库单 {} 没有供应商（supplier_id 为空），复制出来的采购入库单不合法：\
             请先给原单补供应商",
            origin.in_no
        )));
    };

    // ② 构造新单：新号 / DRAFT / 关联全清（见文档注释的三条决策）
    let lines: Vec<NewStockInLine> = items
        .iter()
        .map(|item| NewStockInLine {
            material_id: item.material_id,
            unit_id: item.unit_id,
            // 来源采购明细行 = 采购关联的一部分，一起清（否则待收校验会拿旧行去比）
            source_item_id: None,
            qty: item.qty,
            price: item.price,
            price_incl: item.price_incl,
            tax_rate: item.tax_rate,
            amount: item.amount,
            tax_amount: item.tax_amount,
            with_tax_amount: item.with_tax_amount,
            location_id: item.location_id,
            // 实物序列号不复制（原单那批号码已经过账占用）
            serial_ids: Vec::new(),
            remark: item.remark.clone(),
        })
        .collect();
    let totals = Totals::of(&lines);
    // ★ A4 / 设计 §4.5 档 B：复制单是 E4 的「无源」情形 —— 来源单已清空，
    //   所以必须显式打无源标记，并在备注里写明「复制自 IN-xxx」（无源要可枚举、可追溯）。
    //   备注长度按 `MAX_REMARK_LEN` 兜底：追加后超长就只保留原备注（不因为一条标记把复制搞失败）。
    let copy_mark = format!("| 复制自 {}", origin.in_no);
    let remark = match origin.remark.as_deref().map(str::trim) {
        None | Some("") => copy_mark,
        Some(text) => {
            let merged = format!("{text} {copy_mark}");
            if merged.chars().count() > crate::business::erp::stockin_dto::MAX_REMARK_LEN {
                text.to_string()
            } else {
                merged
            }
        }
    };
    let new = NewStockIn {
        supplier_id,
        warehouse_id: origin.warehouse_id,
        source_order_id: None,
        is_sourceless: true,
        in_date: crate::business::erp::stockin_dto::today_cst(),
        invoice_received: origin.invoice_received,
        remark: Some(remark.as_str()),
    };

    // ③ 插入新单 + 明细（`create` 内部取号 + 写建单日志，与正常建单同一条路径）
    match stockin_model::create(&tx, &new, &lines, &totals, op, now).await {
        Ok(new_id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交复制事务"))?;
            Ok(new_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "复制收货入库单"))
        }
    }
}

/// 复制为新单（`POST /business/erp/stock-in/{id}/copy`，权限 = 建单权限）。
///
/// 响应与建单完全一致（新单的详情），前端拿到就能直接打开编辑弹窗。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/copy",
    method = "POST",
    permission = "business:erp:stock-in:create"
)]
#[nik::post("/business/erp/stock-in/{id}/copy")]
pub async fn stock_in_copy(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StockInDetailRes> {
    let origin_id = parse_uuid(&id, "收货入库单 id")?;
    let new_id = copy_stock_in(origin_id, &claims).await?;
    Ok(Res::ok(detail_of(new_id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
///
/// # P10 铁律 ④：入账后只读
///
/// 命中入账态（`STOCK_IN` = `POSTED`）时：
///
/// * 请求里**任何**金额 / 数量 / 明细 / 供应商 / 仓库 / 来源单 / 入库日期 / 发票标志的改动
///   → **409**「单据已入账…不可修改」（[`state_machine::ensure_writable`] 的统一文案）；
/// * 只改 `remark`（白名单）→ 放行，走 [`stockin_model::update_remark`] 单列更新。
///
/// 判据是**服务端逐字段比对库里现值**，不是看前端有没有藏按钮。
pub async fn update_stock_in(
    id: Uuid,
    req: &CreateStockInReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let current = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;

    if state_machine::Entity::StockIn.is_locked(&current.status) {
        ensure_derived_totals(&current, req, true)?;
        ensure_remark_only(&current, req).await?;
        stockin_model::update_remark(db(), id, req.remark.as_deref(), operator, Utc::now())
            .await
            .map_err(|error| write_error(error, "编辑收货入库单备注"))?;
        return Ok(());
    }

    if current.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）能编辑：已过账的单据已经加过库存，\
             要改只能作废后重开",
            status_label(&current.status),
            current.status
        )));
    }

    // 草稿态：合计同样不许由客户端指定（明细才是唯一真相源）
    ensure_derived_totals(&current, req, false)?;

    let mut request = req.clone();
    resolve_purchase_order(&mut request)?;
    let source_order_id =
        parse_optional_uuid(request.source_order_id.as_ref(), "purchase_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;
    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // ★ A4：草稿改单也走同一个取值点。
    //   ① 请求没带 `is_sourceless` 且这张单本来就是无源单（`current.is_sourceless`）→ 沿用它，
    //      免得「改个备注 / 改个数量」还要每次重申无源身份；
    //   ② 请求带了 ≠ 库里的值 → 按请求走：有源单解绑必须显式声明无源，无源单挂上采购单
    //      则供应商随之重新派生（不允许单独把供应商从 A 换成 B）。
    let declared_sourceless = request
        .is_sourceless
        .or_else(|| (source_order_id.is_none() && current.is_sourceless).then_some(true));
    let pointer = resolve_parent_pointer(
        source_order_id,
        declared_sourceless,
        prepared.supplier_id_raw.as_deref(),
        prepared.remark.as_deref(),
        warehouse_id,
    )
    .await?;
    let supplier_id = pointer.supplier_id;
    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        is_sourceless: pointer.is_sourceless,
        in_date: prepared.in_date,
        invoice_received: prepared.invoice_received,
        remark: prepared.remark.as_deref(),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    match stockin_model::update(&tx, id, &new, &lines, &totals, operator, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(
                "收货入库单状态在本次编辑期间已被改动（只有草稿能改），请刷新后重试",
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
            Err(write_error(error, "编辑收货入库单"))
        }
    }
}

/// 编辑收货入库单（仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "PUT",
    permission = "business:erp:stock-in:update"
)]
#[nik::put("/business/erp/stock-in/{id}")]
pub async fn stock_in_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreateStockInReq>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    update_stock_in(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 状态流转（过账 / 作废） ====================

/// 删除核心逻辑：**只有 `DRAFT`** 能删（软删）。
///
/// ★ P16 反向兜底（P11 的教训）：删除**同一事务**里还要把这张单在跑的作废审批
/// **一并终止**（`stockin_bridge::cancel_approval_of`）。DRAFT 单据按当前状态机不可能
/// 挂着审批单，但这条兜底是仓库「删单 ≠ 留孤儿待办」的机器化承诺 —— 哪天放开
/// 「审批中的单据也能删」，这里不会漏。
pub async fn delete_stock_in(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    // P-FLOW-3A：`PENDING_POST`（过账审批中）也允许删 —— 审批单会被同一事务终止，
    // 不留孤儿待办（P11 教训）。除这两种状态外，单据是库存/成本链的事实，只能作废。
    if row.status != STATUS_DRAFT && row.status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）或过账审批中（{STATUS_PENDING_POST}）能删：\
             已过账的单据是库存与成本链的事实，只能作废（{STATUS_PENDING_CANCEL} 状态下请先撤回作废申请）",
            status_label(&row.status),
            row.status
        )));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除事务"))?;
    let outcome = async {
        // ① 先撤在跑的审批（作废审批 / 过账审批都按 biz_id 找；幂等：没有就什么都不做）
        stockin_bridge::cancel_approval_of(&tx, id, &row.in_no, "收货单已删除，审批自动终止")
            .await
            .map_err(|error| write_error(error, "删除：终止在跑的审批"))?;
        // ② 再软删单据
        let affected = stockin_model::soft_delete(&tx, id, operator, now)
            .await
            .map_err(|error| utils::db_error(error, "软删收货入库单"))?;
        if affected == 0 {
            return Err(not_found("收货入库单"));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交删除事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 删除收货入库单（软删；仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "DELETE",
    permission = "business:erp:stock-in:delete"
)]
#[nik::delete("/business/erp/stock-in/{id}")]
pub async fn stock_in_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_stock_in(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== ⑤ 列表 / 详情 ====================
