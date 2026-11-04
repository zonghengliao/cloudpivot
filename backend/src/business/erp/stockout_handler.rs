// ============================================================================
// stockout_handler.rs —— 出库单接口：建 / 改 / 删 / 详情 / 分页 / 状态流转
//
// 过账（`DRAFT | PENDING_POST → POSTED`）与作废（`POSTED → CANCELLED`）是**唯一动账**的两个
// 入口，都在**一个事务**里做完，任一步失败整笔回滚：
//
// ```text
// 过账：锁单头 → 期间闸 → 状态机留痕 + CAS → 逐行(负库存闸 → 成本账 − → 库位账 − → OUT 流水)
//       → 回写单头金额 / 明细成本快照 → 成本结转凭证 → 回写 voucher_id
// 作废：锁单头 → 状态机留痕 + CAS → 逐行(成本账 + / 库位账 + / IN 冲销流水)
//       → 红字凭证 → 回写 voucher_id（指向红字）
// ```
//
// # 为什么负库存闸在「锁了成本账行之后」
//
// 可用量必须是**本事务锁住的那一行**的值。先在事务外读一遍再更新，两个并发出库
// 都能读到「够」，于是双双过账，账面变负（`40P01` 都拦不住这种逻辑竞态）。
// 锁读 + 判 + 更新三步在同一个事务里，第二个请求必然看到第一个的结果。
//
// # 2026-11 卡面差异（写在这里免得下游猜）
//
// ARCH-3 DC-13 的验收只要求「过账 / 负库存拦 / 作废回滚 / 巡检绿」。本实现在此基础上
// 把 `PENDING_POST` 做成**真的可用状态**（提交待过账 → 他人过账）：
// 出库还没有种子审批流程，所以**不接 flow engine**（不新增 `biz_type`/不种流程），
// 用「状态交接 + `erp.separation_of_duties.post` 内控开关」实现「制单 ≠ 过账」。
// 没有静默降级：本模块不声明任何审批流程，也就不存在「没配流程就直接生效」。
// ============================================================================

use std::collections::HashMap;

use axum::{Extension, Json};
use nik::prelude::chrono::{Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::separation;
use crate::business::erp::voucher;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

use super::stockout_base::*;
use super::stockout_dto::{
    CreateStockOutReq, StockOutDetailRes, StockOutEnvelope, StockOutPageQuery, StockOutPageRes,
    StockOutStatusReq, ensure_draft, list_res, materials_of,
};
use super::stockout_model::{self as stock_out, MaterialInfo};

/// 解析路径 / 请求里的 uuid。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|err| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 不是合法 UUID：{err}"),
        )
    })
}

/// 可选 uuid（空串 / `None` → `None`；非法 → 400）。
fn optional_uuid(raw: Option<&String>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(|text| text.trim()).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn negative_stock(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_STOCK_OUT_NEGATIVE, message.into())
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// `DbError` → `ApiError`（唯一口径：写库错误按约束类型翻译）。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "{ctx}：违反唯一约束{}（单号或行号冲突，请刷新重试）",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::InvalidValue(message) => {
            if let Some(text) = message.strip_prefix(NEGATIVE_PREFIX) {
                ApiError::new(409, error_code::ERP_STOCK_OUT_NEGATIVE, text.to_string())
            } else {
                ApiError::new(400, error_code::PARAM_INVALID, message)
            }
        }
        other => utils::db_error(other, ctx),
    }
}

/// 负库存哨兵前缀：走 `DbError::InvalidValue` 传上来，由 [`write_error`] 翻成 409 + 66034。
const NEGATIVE_PREFIX: &str = "NEGATIVE|";

// ==================== 建单 / 改单 ====================

/// 把请求编译成「单头 + 明细」（校验 + 派生单位 + 库位归属），**不写库**。
///
/// 建单与改单共用这一份，口径只可能有一处漂移。
async fn plan(
    req: &CreateStockOutReq,
) -> std::result::Result<
    (
        String,
        Uuid,
        nik::prelude::chrono::NaiveDate,
        Vec<NewStockOutLine>,
        HashMap<Uuid, MaterialInfo>,
    ),
    ApiError,
> {
    req.check_texts()?;
    req.check_items()?;
    let out_type = req.out_type_value()?;
    let warehouse_id = optional_uuid(req.warehouse_id.as_ref(), "warehouse_id")?
        .ok_or_else(|| bad_request("warehouse_id 不能为空：出库必须指定仓库"))?;
    stock_out::warehouse_policy(db(), warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    let out_date = req.out_date_value();

    let mut lines = Vec::with_capacity(req.items.len());
    let mut materials: HashMap<Uuid, MaterialInfo> = HashMap::new();
    for (index, item) in req.items.iter().enumerate() {
        let line_no = index + 1;
        let material_id = parse_uuid(&item.material_id, "material_id")?;
        let info = match materials.get(&material_id) {
            Some(info) => info.clone(),
            None => stock_out::material_info(db(), material_id)
                .await
                .map_err(|error| utils::db_error(error, "查询物料"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "第 {line_no} 行的物料 {material_id} 不存在或已删除"
                    ))
                })?,
        };
        // 一物一码：账面数量必须等于在库序列号件数（不变式②），
        // 那条件数只由 serial_handler 维护 —— 走出库单会把它冲成负数，直接拦。
        if info.serial_required {
            return Err(bad_request(format!(
                "第 {line_no} 行的物料 {} 是一物一码物料：请走「序列号管理 · 出库 / 报废」\
                 （PATCH /business/erp/serial/{{id}}/status），本出库单只处理普通物料 \
                 —— 否则账面数量会与在库序列号件数对不上",
                info.label()
            )));
        }
        // 单位由物料基础单位带出（DC-09：单据禁用非基本单位，本表干脆不让传单位）
        let location_id = optional_uuid(item.location_id.as_ref(), "location_id")?;
        if let Some(row_id) = location_id {
            let reference = stock_out::location_ref(db(), row_id)
                .await
                .map_err(|error| utils::db_error(error, "查询库位账行"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "第 {line_no} 行的 location_id（{row_id}）不是有效的库位账行：\
                         它指向 biz_inventory_location.id（库位库存行），不是 biz_location.id"
                    ))
                })?;
            if reference.0 != material_id || reference.1 != warehouse_id {
                return Err(bad_request(format!(
                    "第 {line_no} 行的 location_id 指向的库位账行属于物料 {} / 仓库 {}，\
                     与本行的物料 {} / 仓库 {} 不一致",
                    reference.0, reference.1, material_id, warehouse_id
                )));
            }
        }
        lines.push(NewStockOutLine {
            material_id,
            unit_id: info.unit_id,
            qty: item.qty.round_dp(4),
            location_id,
            lot_no: item.lot_no.clone(),
            remark: item.remark.clone(),
        });
        materials.insert(material_id, info);
    }
    if out_type == OUT_TYPE_SCRAP || out_type == OUT_TYPE_LOSS {
        // 报废 / 盘亏必须写原因：这两类要挂 1901 待处理财产损溢，财务要能对上「为什么损了」
        if req
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .is_none()
        {
            return Err(bad_request(format!(
                "{}单必须写 reason（事由 / 原因）：{} 要挂 1901 待处理财产损溢，\
                 没有原因无法向审计解释这笔损失",
                out_type_label(&out_type),
                out_type_label(&out_type)
            )));
        }
    }
    Ok((out_type, warehouse_id, out_date, lines, materials))
}

/// 建单核心（endpoint 与单测共用，只跳过认证）。
pub async fn create_stock_out(
    req: &CreateStockOutReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let (out_type, warehouse_id, out_date, lines, _) = plan(req).await?;
    let totals = Totals::of(&lines);
    let new = NewStockOut {
        out_type: &out_type,
        warehouse_id,
        out_date,
        reason: req.reason.as_deref(),
        remark: req.remark.as_deref(),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match stock_out::create(&tx, &new, &lines, &totals, op, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建出库单"))
        }
    }
}

/// 改单核心（**仅 DRAFT**）。
pub async fn update_stock_out(
    id: Uuid,
    req: &CreateStockOutReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let (out_type, warehouse_id, out_date, lines, _) = plan(req).await?;
    let totals = Totals::of(&lines);
    let new = NewStockOut {
        out_type: &out_type,
        warehouse_id,
        out_date,
        reason: req.reason.as_deref(),
        remark: req.remark.as_deref(),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    let outcome = async {
        let row = stock_out::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定出库单"))?
            .ok_or_else(|| not_found("出库单"))?;
        ensure_draft(&row.status)?;
        let affected = stock_out::update(&tx, id, &new, &lines, &totals, op.id, now)
            .await
            .map_err(|error| write_error(error, "编辑出库单"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "出库单 {} 的状态在本次编辑期间被改动，请刷新后重试",
                row.out_no
            )));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交改单事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 软删核心（**仅 DRAFT**）。
pub async fn delete_stock_out(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除事务"))?;
    let outcome = async {
        let row = stock_out::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定出库单"))?
            .ok_or_else(|| not_found("出库单"))?;
        ensure_draft(&row.status)?;
        let affected = stock_out::soft_delete(&tx, id, op.id, now)
            .await
            .map_err(|error| write_error(error, "删除出库单"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "出库单 {} 的状态在本次删除期间被改动，请刷新后重试",
                row.out_no
            )));
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

// ==================== 过账 ====================

/// 过账核心：`DRAFT | PENDING_POST → POSTED`（三本账负向 + 成本结转凭证，同一事务）。
///
/// # 时序（每一步失败都整笔回滚）
///
/// ```text
/// ① 锁单头（并发重复过账在这里串行化）
/// ①b 期间闸：出库日期所属会计期间已结账 → 409（错在入口，不留半张凭证）
/// ①c 制单 ≠ 过账（受 erp.separation_of_duties.post 开关约束，默认关闭）
/// ② 状态机唯一入口（边表 + 写 biz_status_log）→ CAS 改状态
/// ③ 逐行（已按 material_id / location / line_no 排好序）：
///     锁成本账行 → 负库存闸 → 成本账 − → 锁库位账行 → 负库存闸 → 库位账 − → OUT 流水
///     → 明细回写 unit_cost / amount
/// ④ 单头 total_amount
/// ⑤ 成本结转凭证（借方 6401 / 1901，贷 1405；差额 ≤ 0 跳过）→ 回写 voucher_id
/// ```
pub async fn post_stock_out(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
    reason: Option<&str>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账事务"))?;
    let outcome = post_out_tx(&tx, id, &op, reason, now).await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn post_out_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: &state_machine::Operator,
    reason: Option<&str>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 锁单头
    let header = stock_out::lock(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定出库单"))?
        .ok_or_else(|| not_found("出库单"))?;
    let from_status = header.status.clone();
    if from_status != STATUS_DRAFT && from_status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "出库单 {} 当前是「{}」（{}），不能过账：只有草稿或待过账可以过账（账只减一次）",
            header.out_no,
            status_label(&from_status),
            from_status
        )));
    }

    // ①b 期间闸：已结账的月份只读（凭证层也有闸，这里提前给出可读的错误）
    if let Some((period, status)) = stock_out::period_closed(ex, header.out_date)
        .await
        .map_err(|error| utils::db_error(error, "查询出库日期所属会计期间"))?
        && status == voucher::PERIOD_CLOSED
    {
        return Err(bad_request(format!(
            "出库单 {} 的出库日期 {} 属于已结账的会计期间 {period}：已封账的月份只读，\
             请改到未结账的期间（或先反结账）再过账",
            header.out_no, header.out_date
        )));
    }

    // ①c 制单 ≠ 过账（可配开关，默认关闭；打开后制单人不能自己过账）
    separation::guard(
        separation::Duty::Post,
        header.create_by,
        operator.id,
        &format!("出库单 {}", header.out_no),
    )
    .await?;

    let items = stock_out::items_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "出库单 {} 没有任何明细，不能过账",
            header.out_no
        )));
    }

    // ② 状态机唯一入口 + CAS
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_OUT,
        id,
        &header.out_no,
        &LEGAL_EDGES,
        &from_status,
        STATUS_POSTED,
        operator.clone(),
        reason,
        None,
    )
    .await?;
    let affected = stock_out::apply_status(
        ex,
        id,
        &from_status,
        STATUS_POSTED,
        reason,
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "出库单过账改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "出库单 {} 的状态在本次过账期间被改动（期望 {}），请刷新后重试",
            header.out_no, from_status
        )));
    }

    // ③ 逐行：三本账负向
    let policy = stock_out::warehouse_policy(ex, header.warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库负库存策略"))?
        .ok_or_else(|| reference_missing("出库单的仓库不存在或已删除"))?;
    let biz_type = biz_type_of(&header.out_type);
    let mut total_amount = Decimal::ZERO;
    for item in &items {
        let material = stock_out::material_info(ex, item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?;
        let label = material
            .as_ref()
            .map(MaterialInfo::label)
            .unwrap_or_else(|| item.material_id.to_string());

        let Some(bucket) =
            stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, item.location_id)
                .await
                .map_err(|error| utils::db_error(error, "锁定库存行"))?
        else {
            return Err(negative_stock(format!(
                "库存不足，过账已拒绝：物料 {label} 在仓库 {}（{}）该库位桶上没有库存行（可用 0），\
                 本次要出库 {} —— 请先入库，或改到有库存的库位",
                policy.warehouse_code,
                policy.warehouse_name,
                item.qty.round_dp(4)
            )));
        };

        // 负库存闸（成本账）
        if !policy.allow_negative && bucket.qty < item.qty {
            return Err(negative_stock(format!(
                "库存不足，过账已拒绝：物料 {label} 在仓库 {}（{}）本次要出库 {}，\
                 账面可用 {}（该库位桶）；该仓未开启「允许负库存」",
                policy.warehouse_code,
                policy.warehouse_name,
                item.qty.round_dp(4),
                bucket.qty.round_dp(4)
            )));
        }

        let applied = stock_out::deduct_cost(ex, &bucket, item.qty, now)
            .await
            .map_err(|error| write_error(error, "过账：扣减成本账"))?;

        // 库位账（数量账）：与成本账同一事务同步减，否则 B1（Σ库位 = 成本账）立刻红
        let Some((location_row_id, location_qty)) = stock_out::lock_location_row(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定库位账行"))?
        else {
            return Err(negative_stock(format!(
                "物料 {label} 在仓库 {}（{}）的库位账缺行（明细没指定库位且该仓没有未分配库位桶，\
                 或指定的库位账行已被删除）：账账已经不一致，请先跑库存巡检修复库位账",
                policy.warehouse_code, policy.warehouse_name
            )));
        };
        if !policy.allow_negative && location_qty < item.qty {
            return Err(negative_stock(format!(
                "库存不足，过账已拒绝：物料 {label} 在仓库 {}（{}）该库位的库位账只有 {}，\
                 本次要出库 {}（库位账与成本账口径不一致时取更小者）",
                policy.warehouse_code,
                policy.warehouse_name,
                location_qty.round_dp(4),
                item.qty.round_dp(4)
            )));
        }
        stock_out::deduct_location_qty(ex, location_row_id, item.qty, now)
            .await
            .map_err(|error| write_error(error, "过账：扣减库位账"))?;

        // 流水（OUT，数量恒正；before/after 用回读的真实值）
        stock_out::write_txn(
            ex,
            item,
            header.warehouse_id,
            id,
            &header.out_no,
            biz_type,
            bucket.qty,
            bucket.amount,
            bucket.avg_cost,
            applied.after_qty,
            applied.after_amount,
            applied.after_cost,
            bucket.avg_cost,
            applied.amount,
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：写库存流水"))?;

        // 明细回写过账成本快照（作废时按它原路退回）
        stock_out::set_item_cost(
            ex,
            item.id,
            bucket.avg_cost.round_dp(6),
            applied.amount,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：回写明细成本"))?;
        total_amount += applied.amount;
    }

    // ④ 单头金额
    let total_amount = total_amount.round_dp(4);
    stock_out::set_total_amount(ex, id, total_amount, now)
        .await
        .map_err(|error| write_error(error, "过账：回写单头金额"))?;

    // ⑤ 成本结转凭证（借 6401 / 1901，贷 1405；差额 ≤ 0 内部跳过并 WARN）
    let (debit_code, debit_name) = debit_account_of(&header.out_type);
    let summary = format!(
        "{}出库 {} 结转成本 {}",
        out_type_label(&header.out_type),
        header.out_no,
        total_amount
    );
    // 物料辅助核算：整单只有**一种**物料时才有值。多物料出库的成本结转凭证是一条
    // 汇总行（借 6401 / 贷 1405），把整笔成本挂到首个物料上会让物料成本报表虚增。
    let distinct_materials = {
        let mut materials: Vec<Uuid> = items.iter().map(|item| item.material_id).collect();
        materials.sort_unstable();
        materials.dedup();
        materials.len()
    };
    let aux_material_id = if distinct_materials <= 1 {
        items.first().map(|item| item.material_id)
    } else {
        tracing::warn!(
            out_no = %header.out_no,
            material_count = distinct_materials,
            "出库单含 {} 种物料，成本结转凭证是汇总行 ⇒ aux_material_id 留空（按物料拆分录待 P2-1b 决议）",
            distinct_materials
        );
        None
    };
    let voucher_id = voucher::create_for_stock_out(
        ex,
        &voucher::StockOutVoucher {
            doc_id: id,
            doc_no: &header.out_no,
            voucher_date: header.out_date,
            out_type: &header.out_type,
            debit_code,
            debit_name,
            aux_material_id,
            warehouse_id: header.warehouse_id,
            amount: total_amount,
            summary,
        },
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "过账：生成成本结转凭证"))?;
    if let Some(voucher_id) = voucher_id {
        stock_out::link_voucher(ex, id, voucher_id, now)
            .await
            .map_err(|error| write_error(error, "过账：回写凭证引用"))?;
    }
    tracing::info!(
        stock_out_id = %id,
        out_no = %header.out_no,
        out_type = %header.out_type,
        total_qty = %header.total_qty,
        total_amount = %total_amount,
        voucher = voucher_id.map(|v| v.to_string()).unwrap_or_default(),
        "出库单过账完成：三本账已负向，成本结转凭证已生成"
    );
    Ok(())
}

// ==================== 作废 ====================

/// 作废核心：`DRAFT | PENDING_POST → CANCELLED`（账没动过）与
/// `POSTED → CANCELLED`（三本账回滚 + 红字凭证）两条路。
pub async fn cancel_stock_out(
    id: Uuid,
    reason: &str,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    if reason.trim().is_empty() {
        return Err(bad_request(
            "作废必须写原因（reason）：原因要进状态日志，已过账的还要进红字凭证摘要",
        ));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废事务"))?;
    let outcome = cancel_out_tx(&tx, id, reason.trim(), &op, now).await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn cancel_out_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: &str,
    operator: &state_machine::Operator,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let header = stock_out::lock(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定出库单"))?
        .ok_or_else(|| not_found("出库单"))?;
    let from_status = header.status.clone();
    if from_status != STATUS_DRAFT
        && from_status != STATUS_PENDING_POST
        && from_status != STATUS_POSTED
    {
        return Err(conflict(format!(
            "出库单 {} 当前是「{}」（{}），不能再作废",
            header.out_no,
            status_label(&from_status),
            from_status
        )));
    }

    // 状态机留痕 + CAS（作废原因必填，已过账这条边由边表强制）
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_OUT,
        id,
        &header.out_no,
        &LEGAL_EDGES,
        &from_status,
        STATUS_CANCELLED,
        operator.clone(),
        Some(reason),
        None,
    )
    .await?;
    let affected = stock_out::apply_status(
        ex,
        id,
        &from_status,
        STATUS_CANCELLED,
        Some(reason),
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "出库单作废改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "出库单 {} 的状态在本次作废期间被改动（期望 {}），请刷新后重试",
            header.out_no, from_status
        )));
    }

    // 草稿 / 待过账：从未动过账，到此为止
    if from_status != STATUS_POSTED {
        tracing::info!(
            stock_out_id = %id,
            out_no = %header.out_no,
            from = %from_status,
            "出库单作废（未过账，三本账未动）"
        );
        return Ok(());
    }

    // 已过账 → 三本账回滚（数量与金额都按过账时写下的事实原路退回）
    let items = stock_out::items_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单明细"))?;
    let txn = stock_out::txn_report(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询过账流水"))?;
    if txn.len() != items.len() {
        return Err(ApiError::new(
            409,
            error_code::ERP_STOCK_OUT_CANCEL_BLOCKED,
            format!(
                "出库单 {} 的过账流水缺行（明细 {} 行 / 流水 {} 行）：无法确定当时扣了多少，\
                 拒绝按当前均价估算。请先用数据修复脚本补流水，或走人工红字调整",
                header.out_no,
                items.len(),
                txn.len()
            ),
        ));
    }
    for item in &items {
        // 锁成本账行（拿 before 快照写冲销流水），再加回
        let before =
            stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, item.location_id)
                .await
                .map_err(|error| utils::db_error(error, "锁定库存行"))?
                .unwrap_or(stock_out::BucketSnapshot {
                    inventory_id: Uuid::nil(),
                    qty: Decimal::ZERO,
                    amount: Decimal::ZERO,
                    avg_cost: Decimal::ZERO,
                });
        stock_out::restore_cost(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            item.qty,
            item.amount,
            now,
        )
        .await
        .map_err(|error| write_error(error, "作废：回滚成本账"))?;
        stock_out::restore_location_qty(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            item.qty,
            now,
        )
        .await
        .map_err(|error| write_error(error, "作废：回滚库位账"))?;
        // 回滚后的真实账面（回读，不用 Rust 里算的 before + delta）
        let after =
            stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, item.location_id)
                .await
                .map_err(|error| utils::db_error(error, "读取回滚后的库存行"))?
                .unwrap_or(before);
        stock_out::write_cancel_txn(
            ex,
            item,
            header.warehouse_id,
            id,
            &header.out_no,
            before.qty,
            before.amount,
            before.avg_cost,
            after.qty,
            after.amount,
            after.avg_cost,
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "作废：写冲销流水"))?;
    }

    // 红字凭证（找不到原凭证 = 当时零成本跳过了凭证，不阻断作废）
    let voucher_date = (Utc::now() + Duration::hours(8)).date_naive();
    let red = voucher::reverse_by_source(
        ex,
        voucher::SOURCE_STOCK_OUT,
        id,
        voucher_date,
        Some(reason),
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "作废：红冲成本结转凭证"))?;
    if let Some(red_id) = red {
        stock_out::link_voucher(ex, id, red_id, now)
            .await
            .map_err(|error| write_error(error, "作废：回写红字凭证引用"))?;
    }
    tracing::info!(
        stock_out_id = %id,
        out_no = %header.out_no,
        red_voucher = red.map(|v| v.to_string()).unwrap_or_default(),
        "出库单作废：三本账已回滚，原凭证已红冲"
    );
    Ok(())
}

// ==================== 状态流转入口 ====================

/// 状态流转（`PATCH /status` 与单测共用）。
///
/// | 目标 | 允许的起始态 | 动作 |
/// |---|---|---|
/// | `PENDING_POST` | DRAFT | 提交待过账（只改状态，账没动） |
/// | `POSTED` | DRAFT / PENDING_POST | 过账（三本账负向 + 凭证） |
/// | `DRAFT` | PENDING_POST | 撤回 / 驳回（打回草稿） |
/// | `CANCELLED` | DRAFT / PENDING_POST / POSTED | 作废（已过账的走回滚 + 红冲） |
pub async fn change_status(
    id: Uuid,
    req: &StockOutStatusReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let target = req.target()?;
    // 先用边表把「这张单现在根本不能去那个状态」挡在入口：
    // 报文比状态机的通用文案更贴单据（带单号与中文状态名），
    // 也让「已作废的单再提交」这类误操作不改一行库就返回。
    let current = stock_out::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单"))?
        .ok_or_else(|| not_found("出库单"))?;
    if !can_transition(&current.status, &target) {
        return Err(conflict(format!(
            "出库单 {}：{}",
            current.out_no,
            transition_rejected_message(&current.status, &target)
        )));
    }
    match target.as_str() {
        STATUS_POSTED => post_stock_out(id, op, req.reason_text()).await,
        STATUS_CANCELLED => {
            let reason = req.reason_text().unwrap_or_default();
            cancel_stock_out(id, reason, op).await
        }
        STATUS_PENDING_POST => submit_pending(id, op, req.reason_text()).await,
        STATUS_DRAFT => withdraw_pending(id, op, req.reason_text()).await,
        other => Err(bad_request(format!("不支持的目标状态 {other}"))),
    }
}

/// `DRAFT → PENDING_POST`：提交待过账（账一分没动）。
async fn submit_pending(
    id: Uuid,
    operator: state_machine::Operator,
    reason: Option<&str>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启提交事务"))?;
    let outcome = async {
        let header = stock_out::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定出库单"))?
            .ok_or_else(|| not_found("出库单"))?;
        if header.status != STATUS_DRAFT {
            return Err(conflict(format!(
                "出库单 {} 当前是「{}」（{}），只有草稿可以提交待过账",
                header.out_no,
                status_label(&header.status),
                header.status
            )));
        }
        let items = stock_out::items_of(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "查询出库单明细"))?;
        if items.is_empty() {
            return Err(bad_request(format!(
                "出库单 {} 没有任何明细，不能提交",
                header.out_no
            )));
        }
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_OUT,
            id,
            &header.out_no,
            &LEGAL_EDGES,
            STATUS_DRAFT,
            STATUS_PENDING_POST,
            operator.clone(),
            reason,
            None,
        )
        .await?;
        let affected = stock_out::apply_status(
            &tx,
            id,
            STATUS_DRAFT,
            STATUS_PENDING_POST,
            reason,
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "提交出库单待过账"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "出库单 {} 的状态在本次提交期间被改动，请刷新后重试",
                header.out_no
            )));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// `PENDING_POST → DRAFT`：撤回 / 驳回（打回草稿，账一分没动）。
async fn withdraw_pending(
    id: Uuid,
    operator: state_machine::Operator,
    reason: Option<&str>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启撤回事务"))?;
    let outcome = async {
        let header = stock_out::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定出库单"))?
            .ok_or_else(|| not_found("出库单"))?;
        if header.status != STATUS_PENDING_POST {
            return Err(conflict(format!(
                "出库单 {} 当前是「{}」（{}），只有待过账可以撤回 / 驳回",
                header.out_no,
                status_label(&header.status),
                header.status
            )));
        }
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_OUT,
            id,
            &header.out_no,
            &LEGAL_EDGES,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            operator.clone(),
            reason,
            None,
        )
        .await?;
        let affected = stock_out::apply_status(
            &tx,
            id,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            reason,
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "撤回到草稿"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "出库单 {} 的状态在本次撤回期间被改动，请刷新后重试",
                header.out_no
            )));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 详情 / 分页 ====================

/// 详情核心（endpoint 与单测共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<StockOutDetailRes, ApiError> {
    let row = stock_out::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单"))?
        .ok_or_else(|| not_found("出库单"))?;
    let items = stock_out::items_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单明细"))?;
    let material_ids: Vec<Uuid> = items.iter().map(|item| item.material_id).collect();
    let materials = materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询物料"))?;
    // D3：前端按钮跟着**配置**走 —— 详情里的 `next_statuses` 用有效边（BASE ∩ 配置），
    // 不再是代码里的边表；配置禁用 `DRAFT → POSTED` 之后前端按钮会跟着消失。
    let edges = state_machine::effective_edges(db(), state_machine::Entity::StockOut).await;
    Ok(super::stockout_dto::detail_of(
        &row, items, &materials, &edges,
    ))
}

/// 分页核心。
pub async fn page_of(q: &StockOutPageQuery) -> std::result::Result<StockOutPageRes, ApiError> {
    let filter = q.to_filter();
    let (rows, total) = stock_out::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单列表"))?;
    Ok(StockOutPageRes {
        list: list_res(&rows),
        total,
        page: filter.page,
        page_size: filter.page_size,
    })
}

// ==================== HTTP 入口 ====================

/// 新建出库单（`status = DRAFT`，单号 `OUT-yyyyMMdd-000001`）。
///
/// # 异常情况
///
/// * 401 未登录；403 缺 `business:erp:stock-out:create`；
/// * 400（`10001` / `66002`）：明细为空 / 数量 ≤ 0 / 类型非法 / 仓库或物料不存在 /
///   库位账行不属于本物料本仓 / 一物一码物料 / 报废·盘亏未写原因；
/// * 409（`66001`）：单号或行号唯一键冲突（并发取号）；
/// * 500：数据库异常。
#[nik::permission(
    path = "/business/erp/stock-out",
    method = "POST",
    permission = "business:erp:stock-out:create"
)]
#[nik::post("/business/erp/stock-out")]
pub async fn stock_out_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateStockOutReq>,
) -> ResResult<StockOutEnvelope> {
    let id = create_stock_out(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 编辑出库单（**仅草稿**，整单覆盖：单头 + 明细删旧插新）。
#[nik::permission(
    path = "/business/erp/stock-out/{id}",
    method = "PUT",
    permission = "business:erp:stock-out:update"
)]
#[nik::put("/business/erp/stock-out/{id}")]
pub async fn stock_out_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreateStockOutReq>,
) -> ResResult<StockOutEnvelope> {
    let id = parse_uuid(&id, "id")?;
    update_stock_out(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 删除出库单（**仅草稿**，软删；已过账的请走作废）。
#[nik::permission(
    path = "/business/erp/stock-out/{id}",
    method = "DELETE",
    permission = "business:erp:stock-out:delete"
)]
#[nik::delete("/business/erp/stock-out/{id}")]
pub async fn stock_out_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_stock_out(id, &claims).await?;
    Ok(Res::ok(()))
}

/// 状态流转：提交待过账 / 过账 / 撤回 / 作废（同一入口，边表决定合法性）。
#[nik::permission(
    path = "/business/erp/stock-out/{id}/status",
    method = "PATCH",
    permission = "business:erp:stock-out:status"
)]
#[nik::patch("/business/erp/stock-out/{id}/status")]
pub async fn stock_out_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StockOutStatusReq>,
) -> ResResult<StockOutEnvelope> {
    let id = parse_uuid(&id, "id")?;
    change_status(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 出库单分页。
#[nik::permission(
    path = "/business/erp/stock-out/page",
    method = "GET",
    permission = "business:erp:stock-out:list"
)]
#[nik::get("/business/erp/stock-out/page")]
pub async fn stock_out_page(Query(q): Query<StockOutPageQuery>) -> ResResult<StockOutPageRes> {
    Ok(Res::ok(page_of(&q).await?))
}

/// 出库单详情（含明细与成本结转金额）。
#[nik::permission(
    path = "/business/erp/stock-out/{id}",
    method = "GET",
    permission = "business:erp:stock-out:detail"
)]
#[nik::get("/business/erp/stock-out/{id}")]
pub async fn stock_out_detail(Path(id): Path<String>) -> ResResult<StockOutDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}
