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

// 出库过账的跨单据联动接线（P1-FIX-5C）：单独一个文件，只为守住单文件 ≤1500 行。
#[path = "stockout_linkage.rs"]
mod linkage_hook;

use crate::business::flow::linkage;

use super::stockout_base::*;
use super::stockout_dto::{
    CreateStockOutReq, StockOutDetailRes, StockOutEnvelope, StockOutPageQuery, StockOutPageRes, StockOutStatusReq,
    ensure_draft, list_res, materials_of,
};
use super::stockout_model::{self as stock_out, MaterialInfo};

/// 解析路径 / 请求里的 uuid。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim())
        .map_err(|err| ApiError::new(400, error_code::PARAM_INVALID, format!("{field} 不是合法 UUID：{err}")))
}

/// 可选 uuid（空串 / `None` → `None`；非法 → 400）。
fn optional_uuid(raw: Option<&String>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(|text| text.trim()).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, format!("{what}不存在或已删除"))
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
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
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
    // 同一张单里「已经预占掉」的桶量：`(物料, 库位账行 id)` → 数量。
    //
    // 为什么需要它：同一物料可以出现在多行（用户分次领用 / 不同批次），每行都跑一次 FIFO
    // 会把同一个桶的货重复分配（A 只有 30，两行各 30 都挑到 A ⇒ 过账第二行必然 409）。
    // 用户手填库位的那一行也登记进来 —— 它占用的量后面的自动行不能再取。
    let mut reserved: HashMap<(Uuid, Option<Uuid>), Decimal> = HashMap::new();
    for (index, item) in req.items.iter().enumerate() {
        let line_no = index + 1;
        let material_id = parse_uuid(&item.material_id, "material_id")?;
        let info = match materials.get(&material_id) {
            Some(info) => info.clone(),
            None => stock_out::material_info(db(), material_id)
                .await
                .map_err(|error| utils::db_error(error, "查询物料"))?
                .ok_or_else(|| reference_missing(format!("第 {line_no} 行的物料 {material_id} 不存在或已删除")))?,
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
        let unit_id = info.unit_id;
        let qty = item.qty.round_dp(4);
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
            // 用户手填的库位 = **覆盖自动值**（卡面 §2）：原样尊重，不再跑 FIFO；
            // 但要登记它占用的桶量，后面的「自动分配」行才不会再从同一个桶里取第二次。
            *reserved.entry((material_id, Some(row_id))).or_insert(Decimal::ZERO) += qty;
            lines.push(NewStockOutLine {
                material_id,
                unit_id,
                qty,
                location_id: Some(row_id),
                lot_no: item.lot_no.clone(),
                remark: item.remark.clone(),
            });
        } else {
            // ★ P1-FIX-2：没指定库位 → **建单就按 FIFO 预填**，跨库位够量时自动拆成多行。
            //
            // 改之前：建单只存「没指定库位」，过账才 FIFO 挑**单个够量**的库位，都不够量就
            // 409 让用户「把这一行拆成多行并分别指定库位」—— 货明明够（分散在 2 个库位），
            // 用户就得手工拆行（评审实测 ③ 段，操作便捷性 3 分的主因）。
            //
            // 现在的口径：
            //   ① 够量 → 按 FIFO 顺序拆成 N 行，每行一个库位账行（一行明细 ↔ 一笔流水不变）；
            //   ② 不够量 → 仍存「未指定库位」单行，草稿可存，过账时 409 给出**缺口 + 分布**；
            //   ③ 手填了库位的行根本不走这里（用户可覆盖自动值）。
            let plan = stock_out::fifo_plan(db(), material_id, warehouse_id, qty)
                .await
                .map_err(|error| utils::db_error(error, "按 FIFO 预分库位"))?;
            let buckets: Vec<stock_out::FifoBucket> = plan
                .buckets
                .iter()
                .map(|bucket| {
                    let used = reserved
                        .get(&(material_id, bucket.location_row_id))
                        .copied()
                        .unwrap_or(Decimal::ZERO);
                    stock_out::FifoBucket {
                        available: (bucket.available - used).max(Decimal::ZERO),
                        ..bucket.clone()
                    }
                })
                .collect();
            let (picks, shortfall) = stock_out::allocate_fifo(&buckets, qty);
            if shortfall <= Decimal::ZERO && !picks.is_empty() {
                for pick in picks {
                    *reserved
                        .entry((material_id, pick.location_row_id))
                        .or_insert(Decimal::ZERO) += pick.take;
                    lines.push(NewStockOutLine {
                        material_id,
                        unit_id,
                        qty: pick.take,
                        location_id: pick.location_row_id,
                        lot_no: item.lot_no.clone(),
                        remark: item.remark.clone(),
                    });
                }
            } else {
                lines.push(NewStockOutLine {
                    material_id,
                    unit_id,
                    qty,
                    location_id: None,
                    lot_no: item.lot_no.clone(),
                    remark: item.remark.clone(),
                });
            }
        }
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
        // 手工建单没有来源单据（销售链的建单走 `shipment_posting`，不经本函数）
        source_type: None,
        source_id: None,
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
        // 手工建单没有来源单据（销售链的建单走 `shipment_posting`，不经本函数）
        source_type: None,
        source_id: None,
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
/// ⑤b 销售出库（`source_type='SALES'`）→ 自动挂应收（`receivable_bridge`，SO-14）
/// ⑥ 跨单据联动（排除库存 / 凭证四类动作；AfterCommit 动作由调用方在 commit 后跑）
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
        Ok(linkage_run) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账事务"))?;
            // 提交**之后**才跑 AfterCommit 动作（发通知 / 建任务）：
            // 它们在独立事务里，失败只记日志，不回滚已经落定的账。
            if let Some(row) = stock_out::find(id)
                .await
                .map_err(|error| utils::db_error(error, "过账后回查出库单"))?
            {
                linkage_hook::run_deferred_linkage(&row, &op, linkage_run).await;
            }
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
) -> std::result::Result<linkage::LinkageRun, ApiError> {
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
        return Err(bad_request(format!("出库单 {} 没有任何明细，不能过账", header.out_no)));
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
    let affected = stock_out::apply_status(ex, id, &from_status, STATUS_POSTED, reason, operator.id, now)
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
    // 流水业务类型：`out_type` 与 `source_type` **一起**看（FIX-7）——
    // 销售出库单的 out_type 固定 OTHER，只按 out_type 会把它记成 OTHER_OUT，
    // 「按 biz_type 统计销售出库」就全漏了。见 `stockout_base::biz_type_of_doc`。
    let biz_type = biz_type_of_doc(&header.out_type, header.source_type.as_deref());
    let mut total_amount = Decimal::ZERO;
    // 按物料聚合的存货行（P2-1b §4.2）：每行的金额是**账上真实扣减额** `applied.amount`，
    // 循环结束后用 `VoucherMaterialLine::normalize` 合并同物料多行（金额、数量相加）。
    // 不在 handler 里另写一套合并：口径只保留一份实现，否则与收货侧迟早出现两种行序。
    let mut material_lines: Vec<voucher::VoucherMaterialLine> = Vec::with_capacity(items.len());
    for item in &items {
        let material = stock_out::material_info(ex, item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?;
        let label = material
            .as_ref()
            .map(MaterialInfo::label)
            .unwrap_or_else(|| item.material_id.to_string());

        // ★ P2-4 出库库位策略：**指定就按指定扣，没指定就 FIFO**
        //
        // 改之前「没指定库位」= 只从「未分配」桶扣：客户把货收到 A 库位后，
        // 不选库位出库会报「库存不足（可用 0）」，明明账上有货 —— 这也是
        // 收货永远进未分配桶的下游症状。现在的口径（与 docs/ERP-库位策略.md 一致）：
        //   ① 明细指定了库位账行 → 精确扣那一桶（校验物料 / 仓库）；
        //   ② 没指定 → 按库位**建账时间**升序 FIFO，挑第一个够量的库位；
        //   ③ 单个库位都不够量 → 409 点名各库位余量，让用户拆行或指定库位
        //      （不静默跨桶拆扣：一行明细 ↔ 一笔流水的对应关系会断）。
        let mut location_row = item.location_id;
        if location_row.is_none() {
            match stock_out::fifo_location_row(
                ex,
                item.material_id,
                header.warehouse_id,
                item.qty,
                policy.allow_negative,
            )
            .await
            .map_err(|error| utils::db_error(error, "FIFO 选库位"))?
            {
                Some((row_key, _)) => {
                    location_row = row_key;
                    // 回写明细：作废时按同一行原路退回（NULL = 未分配桶，同样要写回去）
                    if location_row != item.location_id {
                        stock_out::set_item_location(ex, item.id, row_key, now)
                            .await
                            .map_err(|error| write_error(error, "过账：回写明细库位"))?;
                    }
                }
                None => {
                    // ★ P1-FIX-2：不再把「拆行」甩给用户 —— 文案要说清「差多少 + 现有在哪些库位
                    // + 该怎么办」。改之前这里只报「没有任何一个库位够量，请把这一行拆成多行
                    // 并分别指定库位」，货明明分布在 2 个库位、加起来够，用户也得手工作业。
                    let plan = stock_out::fifo_plan(ex, item.material_id, header.warehouse_id, item.qty)
                        .await
                        .map_err(|error| utils::db_error(error, "统计各库位可用量"))?;
                    let distribution = plan.availability_text();
                    let advice = if plan.available > Decimal::ZERO {
                        format!(
                            "② 把本行数量改到 {} 以内；\
                             ③ 重新保存草稿 —— 系统会按 FIFO 自动把这一行拆成多行、分别占用各库位，\
                             不需要手工拆行",
                            plan.available.round_dp(4)
                        )
                    } else {
                        "② 该仓当前一件可用库存都没有：先收货 / 入库，或改用别的仓库；\
                         ③ 重新保存草稿不影响结论（没货就出不去）"
                            .to_string()
                    };
                    return Err(negative_stock(format!(
                        "库存不足，过账已拒绝：物料 {label} 在仓库 {}（{}）第 {} 行要出库 {}，\
                         该仓可用合计 {}，差 {}；现有库存分布：{}（按库位建账时间 FIFO 排序，\
                         单个库位都不够这一行要的量）。处理办法：\
                         ① 先补货 / 收货后再过账；{advice}",
                        policy.warehouse_code,
                        policy.warehouse_name,
                        item.line_no,
                        item.qty.round_dp(4),
                        plan.available.round_dp(4),
                        plan.shortfall.round_dp(4),
                        distribution
                    )));
                }
            }
        }

        let Some(bucket) = stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, location_row)
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
            // ★ P1-FIX-2：点明「哪个库位 / 差多少 / 全仓还分布在哪」，并给可执行的动作。
            let location = stock_out::location_label(ex, location_row)
                .await
                .map_err(|error| utils::db_error(error, "查询库位编码"))?;
            let plan = stock_out::fifo_plan(ex, item.material_id, header.warehouse_id, item.qty)
                .await
                .map_err(|error| utils::db_error(error, "统计各库位可用量"))?;
            return Err(negative_stock(format!(
                "库存不足，过账已拒绝：物料 {label} 在仓库 {}（{}）库位 {location} 的第 {} 行\
                 要出库 {}，该库位可用 {}，差 {}；该仓未开启「允许负库存」。\
                 该物料在本仓的全部可用：{}（FIFO 序）。处理办法：\
                 ① 先补货 / 收货后再过账；② 减少本行数量；\
                 ③ 清空本行库位（或重新保存草稿）让系统按 FIFO 自动分配 —— \
                 库存分散在多个库位时会自动拆行，不需要手工拆",
                policy.warehouse_code,
                policy.warehouse_name,
                item.line_no,
                item.qty.round_dp(4),
                bucket.qty.round_dp(4),
                (item.qty - bucket.qty).round_dp(4),
                plan.availability_text()
            )));
        }

        let applied = stock_out::deduct_cost(ex, &bucket, item.qty, now)
            .await
            .map_err(|error| write_error(error, "过账：扣减成本账"))?;

        // 库位账（数量账）：与成本账同一事务同步减，否则 B1（Σ库位 = 成本账）立刻红
        let Some((location_row_id, location_qty)) =
            stock_out::lock_location_row(ex, item.material_id, header.warehouse_id, location_row)
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
            stock_out::StockOutTxnInput {
                item,
                warehouse_id: header.warehouse_id,
                doc_id: id,
                doc_no: &header.out_no,
                biz_type,
                before_qty: bucket.qty,
                before_amount: bucket.amount,
                before_cost: bucket.avg_cost,
                after_qty: applied.after_qty,
                after_amount: applied.after_amount,
                after_cost: applied.after_cost,
                unit_cost: bucket.avg_cost,
                amount: applied.amount,
                operator: operator.id,
                now,
            },
        )
        .await
        .map_err(|error| write_error(error, "过账：写库存流水"))?;

        // 明细回写过账成本快照（作废时按它原路退回）
        stock_out::set_item_cost(ex, item.id, bucket.avg_cost.round_dp(6), applied.amount, now)
            .await
            .map_err(|error| write_error(error, "过账：回写明细成本"))?;
        total_amount += applied.amount;
        material_lines.push(voucher::VoucherMaterialLine {
            material_id: item.material_id,
            qty: item.qty,
            amount: applied.amount,
            line_from: item.line_no,
            line_to: item.line_no,
        });
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
    // 按物料拆分录（P2-1b §4.3）：`M` 个物料 ⇒ 借 6401 / 1901 `M` 行 + 贷 1405 `M` 行。
    // 合并 / 升序 / 尾差吸收全部在 `voucher::create_for_stock_out` 里（共享 builder），
    // 这里只负责把「逐行的账上真实扣减额」交过去。
    let material_lines = voucher::VoucherMaterialLine::normalize(material_lines);
    let voucher_id = voucher::create_for_stock_out(
        ex,
        &voucher::StockOutVoucher {
            doc_id: id,
            doc_no: &header.out_no,
            voucher_date: header.out_date,
            out_type: &header.out_type,
            debit_code,
            debit_name,
            material_lines,
            aux_material_id: items.first().map(|item| item.material_id),
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
    // ⑤b 销售出库 → 应收挂账（SO-14）：判据是出库单的 `source_type='SALES'`（V0285），
    //     不是 out_type。与 ②③ 同一事务：挂账失败（客户不存在 / 发货单缺失 / 库层约束）
    //     直接 Err → 调用方回滚，库存与成本凭证一起消失（不许出现「货出了、应收没挂」）。
    //     幂等三道闸在 receivable_bridge 里（已有行 → 跳过；并发撞 `uk_biz_ar_source` → 按已挂上处理）。
    let receivable = crate::business::erp::receivable_bridge::post_in_tx(ex, id, operator.id, now).await?;
    if let Some(bridged) = receivable.as_ref() {
        tracing::info!(
            stock_out_id = %id,
            out_no = %header.out_no,
            ar_no = %bridged.ar_no,
            amount = %bridged.amount,
            created = bridged.created,
            "销售出库过账：应收已挂账"
        );
    }

    tracing::info!(
        stock_out_id = %id,
        out_no = %header.out_no,
        out_type = %header.out_type,
        total_qty = %header.total_qty,
        total_amount = %total_amount,
        voucher = voucher_id.map(|v| v.to_string()).unwrap_or_default(),
        receivable = receivable.as_ref().map(|r| r.ar_no.clone()).unwrap_or_default(),
        "出库单过账完成：三本账已负向，成本结转凭证已生成"
    );

    // ⑥ 跨单据联动（P1-FIX-5C）：出库过账的库存 / 成本 / 凭证都已由 ③⑤ 自己落账，
    //    所以通用引擎必须**显式排除**这四类动作（否则同一行明细扣两次库存、
    //    一张单两张成本结转凭证 / 回写同一份成本快照）。排除清单在
    //    `spec::STOCKOUT_EXCLUDED_ACTIONS`，不是写死在引擎里（别的触发点仍可用通用路线）。
    //
    //    读规则失败 = 空规则集 = 行为与配置前逐字相同（`model::rules_for` fail-safe）；
    //    关键动作失败 → `Err` → 调用方整笔回滚（与本函数其它步骤同一口径）。
    //    提交后动作（发通知 / 建任务）放进 `deferred`，由 `post_stock_out` 在 commit 之后跑。
    let linkage_ctx = linkage_hook::post_linkage_ctx(&header, operator);
    let linkage_run =
        linkage::engine::run_excluding(ex, &linkage_ctx, linkage::spec::STOCKOUT_EXCLUDED_ACTIONS).await?;
    if !linkage_run.is_empty() {
        tracing::info!(
            stock_out_id = %id,
            out_no = %header.out_no,
            in_tx_rules = linkage_run.runs.len(),
            deferred_rules = linkage_run.deferred.len(),
            "出库过账：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(linkage_run)
}

// ==================== 销售发货复用的过账内核（SO-10） ====================

/// 出库过账内核：**调用方负责事务边界与状态机之外的业务后置**。
///
/// 存在的唯一理由是复用：销售发货过账（`shipment_posting`）要在**自己的事务**里
/// 走一遍完全相同的「负库存闸 → 三本账负向 → 成本结转凭证 → 联动」链路，
/// 而不是另写一份（两份口径迟早漂移，一份改了另一份不改 = 账对不上）。
///
/// 返回的 [`linkage::LinkageRun`] 里是**提交后才能跑**的动作；调用方必须在
/// `commit()` 成功之后调 [`run_deferred_linkage_after_commit`]，否则通知 / 待办
/// 会在事务里发出去（回滚后仍然发了）。
pub(super) async fn post_out_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: &state_machine::Operator,
    reason: Option<&str>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<linkage::LinkageRun, ApiError> {
    post_out_tx(ex, id, operator, reason, now).await
}

/// 提交后跑出库过账的延迟联动（发通知 / 建任务，独立事务，失败只记日志）。
pub(super) async fn run_deferred_linkage_after_commit(
    id: Uuid,
    operator: &state_machine::Operator,
    run: linkage::LinkageRun,
) {
    if run.deferred.is_empty() {
        return;
    }
    match stock_out::find(id).await {
        Ok(Some(row)) => linkage_hook::run_deferred_linkage(&row, operator, run).await,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(stock_out_id = %id, %error, "出库过账后回查单据失败：延迟联动已跳过")
        }
    }
}

// ==================== 作废 ====================

/// 作废核心：`DRAFT | PENDING_POST → CANCELLED`（账没动过）与
/// `POSTED → CANCELLED`（三本账回滚 + 红字凭证 + **销售来源冲销应收**）两条路。
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
    if from_status != STATUS_DRAFT && from_status != STATUS_PENDING_POST && from_status != STATUS_POSTED {
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
    let affected = stock_out::apply_status(ex, id, &from_status, STATUS_CANCELLED, Some(reason), operator.id, now)
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
        let before = stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, item.location_id)
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
        let after = stock_out::lock_bucket(ex, item.material_id, header.warehouse_id, item.location_id)
            .await
            .map_err(|error| utils::db_error(error, "读取回滚后的库存行"))?
            .unwrap_or(before);
        stock_out::write_cancel_txn(
            ex,
            stock_out::StockOutCancelTxnInput {
                item,
                warehouse_id: header.warehouse_id,
                doc_id: id,
                doc_no: &header.out_no,
                before_qty: before.qty,
                before_amount: before.amount,
                before_cost: before.avg_cost,
                after_qty: after.qty,
                after_amount: after.amount,
                after_cost: after.avg_cost,
                operator: operator.id,
                now,
            },
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

    // 应收冲销（只有销售来源才有）：与三本账回滚 / 红字凭证**同一个事务**。
    // 顺序与过账侧对称（过账：三本账 → 成本凭证 → 挂应收；作废：回滚三本账 → 红字凭证 → 冲销应收）。
    // 任何 Err（应收已收款 / 已核销 / 并发改状态）都会让整笔作废回滚 —— 单据留在 POSTED，
    // 不允许出现「货退回库存了、应收还挂着」或「应收作废了、库存没回」的半截账。
    let ar = crate::business::erp::receivable_bridge::cancel_in_tx(ex, id, reason, operator.id, now).await?;

    tracing::info!(
        stock_out_id = %id,
        out_no = %header.out_no,
        red_voucher = red.map(|v| v.to_string()).unwrap_or_default(),
        receivable = ar.as_ref().map(|outcome| outcome.ar_no.clone()).unwrap_or_default(),
        receivable_cancelled = ar.as_ref().is_some_and(|outcome| outcome.cancelled),
        "出库单作废：三本账已回滚，原凭证已红冲，销售来源已冲销应收"
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
            return Err(bad_request(format!("出库单 {} 没有任何明细，不能提交", header.out_no)));
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
        let affected = stock_out::apply_status(&tx, id, STATUS_DRAFT, STATUS_PENDING_POST, reason, operator.id, now)
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
            tx.commit().await.map_err(|error| utils::db_error(error, "提交事务"))?;
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
        let affected = stock_out::apply_status(&tx, id, STATUS_PENDING_POST, STATUS_DRAFT, reason, operator.id, now)
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
            tx.commit().await.map_err(|error| utils::db_error(error, "提交事务"))?;
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
    // ★ P1-FIX-2：草稿 / 待过账的单据附带「哪一行不够、差多少、现有货在哪些库位」。
    let warnings = stock_warnings(&row, &items, &materials).await?;
    let mut detail = super::stockout_dto::detail_of(&row, items, &materials, &edges, warnings);
    // AUD-02：制单人姓名（单条同一入口，1 条 SQL）
    crate::business::shared::user_name::fill_person_names(std::slice::from_mut(&mut detail.header))
        .await
        .unwrap_or_default();
    Ok(detail)
}
/// 逐行比「要出的量」与「这一行能拿到的量」，给草稿 / 待过账单据生成库存提示（P1-FIX-2）。
///
/// 三个口径：
///
/// * **明细没指定库位**（只有「全仓都不够」时建单才会留下这种行）→ 拿该物料该仓的
///   可用合计比，提示里带「差多少 + 现有分布在哪些库位」；
/// * **明细指定了库位**（含建单时 FIFO 自动预填、或用户手改后的值）→ 只拿那个库位的量比，
///   不够就提示「改这个库位 / 清空库位让系统重新按 FIFO 分配」；
/// * **已过账 / 已作废**的单据不提示（账已经动过，提示没有意义）。
///
/// 只提示**不拦截**：草稿可以先建（货在路上），过账时由 409 兜底 —— 这也是「默认自动、
/// 允许手改」的配套：用户可以先把单排出去，看到提示再补货或调数量。
async fn stock_warnings(
    row: &StockOutRow,
    items: &[StockOutItemRow],
    materials: &HashMap<Uuid, MaterialInfo>,
) -> std::result::Result<Vec<String>, ApiError> {
    if row.status != STATUS_DRAFT && row.status != STATUS_PENDING_POST {
        return Ok(Vec::new());
    }
    // 一个物料只查一次库位桶列表（一张出库单通常 1~3 个物料）
    let mut cache: HashMap<Uuid, Vec<stock_out::FifoBucket>> = HashMap::new();
    let mut warnings: Vec<String> = Vec::new();
    for item in items {
        let buckets = match cache.get(&item.material_id) {
            Some(list) => list.clone(),
            None => {
                let list = stock_out::fifo_buckets(db(), item.material_id, row.warehouse_id)
                    .await
                    .map_err(|error| utils::db_error(error, "统计各库位可用量"))?;
                cache.insert(item.material_id, list.clone());
                list
            }
        };
        let label = materials
            .get(&item.material_id)
            .map(MaterialInfo::label)
            .unwrap_or_else(|| item.material_id.to_string());
        let qty = item.qty.round_dp(4);
        match item.location_id {
            Some(row_id) => match buckets.iter().find(|bucket| bucket.location_row_id == Some(row_id)) {
                Some(bucket) if bucket.available >= qty => {}
                Some(bucket) => warnings.push(format!(
                    "物料 {label} 第 {} 行：指定库位 {} 只有 {}，本行要出 {}，差 {}；\
                         该物料在本仓还分布在：{}。清空本行库位（或重新保存草稿）会按 FIFO \
                         自动分配到有货的库位",
                    item.line_no,
                    bucket.location_code,
                    bucket.available.round_dp(4),
                    qty,
                    (qty - bucket.available).round_dp(4),
                    stock_out::describe_buckets(&buckets)
                )),
                None => warnings.push(format!(
                    "物料 {label} 第 {} 行：指定的库位账行已不存在或库存为 0，本行要出 {}；\
                         该物料在本仓可用：{}。请重新选择库位，或清空库位让系统按 FIFO 自动分配",
                    item.line_no,
                    qty,
                    stock_out::describe_buckets(&buckets)
                )),
            },
            None => {
                let available = buckets
                    .iter()
                    .fold(Decimal::ZERO, |acc, bucket| acc + bucket.available)
                    .round_dp(4);
                if available >= qty {
                    continue;
                }
                let advice = if available > Decimal::ZERO {
                    format!(
                        "请先补货 / 收货，或把数量改到 {available} 以内 —— 够量时保存草稿\
                         会按 FIFO 自动拆成多行并分别占用库位"
                    )
                } else {
                    "该仓当前一件可用库存都没有：请先收货 / 入库，或改用别的仓库".to_string()
                };
                warnings.push(format!(
                    "物料 {label} 第 {} 行：要出 {}，本仓可用合计 {}，差 {}；现有库存分布：{}。{}",
                    item.line_no,
                    qty,
                    available,
                    (qty - available).round_dp(4),
                    stock_out::describe_buckets(&buckets),
                    advice
                ));
            }
        }
    }
    Ok(warnings)
}

/// 分页核心。
pub async fn page_of(q: &StockOutPageQuery) -> std::result::Result<StockOutPageRes, ApiError> {
    let filter = q.to_filter();
    let (rows, total) = stock_out::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单列表"))?;
    let mut list = list_res(&rows);
    // AUD-02：制单人姓名，整页一次
    crate::business::shared::user_name::fill_person_names(&mut list)
        .await
        .unwrap_or_default();
    Ok(StockOutPageRes {
        list,
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
