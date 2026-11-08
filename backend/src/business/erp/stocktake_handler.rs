// ============================================================================
// stocktake_handler.rs —— 盘点单接口：建 / 改 / 删 / 状态流转 / 列表 / 详情
//
// 两个**动账**的入口都在一个事务里做完，任一步失败整笔回滚：
//
// ```text
// 过账（PENDING_POST → POSTED）：锁单头 → 期间闸 → 审批结论闸 → 制单≠过账闸
//      → 逐行（盘盈：成本账+ / 库位账+ / IN 流水；盘亏：锁桶+负库存闸+成本账− / 库位账− / OUT 流水）
//      → 单头差异合计 → 盘点差异凭证 → 回写 voucher_id
// 作废（POSTED → CANCELLED）：锁单头 → 状态机留痕 + CAS
//      → 逐行（盘盈行按账上金额冲减 / 盘亏行原路加回 + 反向流水）→ 红冲凭证
// ```
//
// # 提交审批（DRAFT → PENDING_POST）
//
// 走 `flow` 引擎（流程 `stock_take_post`，种子在 `V0211`），**与采购 / 收货 / 合同同一套
// 机制**：有已发布流程就必须走审批（请求里带 `skip_approval` → 400，不许绕过内控）；
// 没有已发布流程 → 默认 409（`approval_gate` 的 fail-closed 口径），
// 只有「持 `business:erp:skip-approval` + 理由 ≥10 字」才放行，理由写进 `biz_status_log`。
//
// # 为什么「审批通过」与「过账」是两次动作
//
// 过账会真的动三本账并出凭证，是**不可撤回**的记账动作（撤回只能作废 + 红冲）。
// 审批只回答「这份差异能不能调平」；过账回答「现在调」。两者之间留一次点击，
// 才有 `posted_by` 这个可追责的人，也才让「制单 / 审批 / 过账」三人可分
// （`erp.separation_of_duties.post` 开关控制制单≠过账）。
// ============================================================================

use axum::{Extension, Json};
use nik::prelude::chrono::{Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::attachment::model::DocKind as AttachDocKind;
use crate::business::attachment::rule::{self as attach_rule, RequiredAt as AttachAt};
use crate::business::erp::approval_gate;
use crate::business::erp::separation;
use crate::business::erp::stockout_model;
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

use super::stocktake_bridge as bridge;
// 联动 hook（LNK-04）。`#[path]` 写在本文件而不是挂到 `erp/mod.rs`：多 worker 并行时
// `mod.rs` 是冲突热点（与 `payable_payment.rs` 的同一做法）。
#[path = "stocktake_linkage.rs"]
mod stocktake_linkage;
use super::stocktake_dto::{
    CreateStockTakeReq, StockTakeDetailRes, StockTakeEnvelope, StockTakePageQuery,
    StockTakePageRes, StockTakeRes, StockTakeStatusReq, UpdateStockTakeReq,
};
use super::stocktake_model::{
    self as model, DiffApplied, NEGATIVE_PREFIX, NewStockTake, NewStockTakeLine, STATUS_CANCELLED,
    STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED, StockTakeItemRow, StockTakeRow, VoucherPair,
    status_label,
};

// ==================== 错误构造 ====================

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

/// `DbError` → `ApiError`：负库存哨兵翻 409，其余按库错误类型翻译。
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
                negative_stock(text.to_string())
            } else if let Some(text) = message.strip_prefix(stockin_shortfall_prefix()) {
                negative_stock(text.to_string())
            } else {
                bad_request(message)
            }
        }
        other => utils::db_error(other, ctx),
    }
}

/// 收货作废的库存不足前缀（盘盈行回滚时由 `stockin_cancel` 抛上来）。
fn stockin_shortfall_prefix() -> &'static str {
    "INV_SHORTFALL|"
}

/// 今天（北京时间）。
fn today_cst() -> nik::prelude::chrono::NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

// ==================== 建单 ====================

/// 建单核心（HTTP 与单测共用）。
///
/// 明细的账面数与成本**由服务端从 `biz_inventory` 快照**，客户端只给范围。
pub async fn create_stock_take(
    req: &CreateStockTakeReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let warehouse_id = req.warehouse()?;
    let take_type = req.take_type_value()?;
    let take_date = req.take_date.unwrap_or_else(today_cst);
    let remark = req.remark_text()?.unwrap_or_default();
    let scope = req.scope_items(&take_type)?;

    let policy = model::warehouse_policy(db(), warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库"))?
        .ok_or_else(|| reference_missing("盘点仓库不存在或已删除"))?;

    // 明细来源：全盘 = 该仓现有全部库存桶；抽盘 = 客户端指定（逐行校验物料 / 库位）。
    let mut lines: Vec<NewStockTakeLine> = Vec::new();
    if take_type == model::TAKE_TYPE_FULL {
        let rows = model::book_rows_of_warehouse(db(), warehouse_id)
            .await
            .map_err(|error| utils::db_error(error, "快照库存桶"))?;
        if rows.is_empty() {
            return Err(bad_request(format!(
                "仓库 {}({}) 当前没有任何库存行（或物料已删除），全盘没有可盘对象：\
                 请改用抽盘，或先入库后再盘",
                policy.warehouse_code, policy.warehouse_name
            )));
        }
        lines = rows;
    } else {
        for (material_id, location_id) in scope {
            let material = stockout_model::material_info(db(), material_id)
                .await
                .map_err(|error| utils::db_error(error, "查询物料"))?
                .ok_or_else(|| reference_missing(format!("物料 {material_id} 不存在或已删除")))?;
            let (book_qty, unit_cost) =
                model::book_of_bucket(db(), material_id, warehouse_id, location_id)
                    .await
                    .map_err(|error| utils::db_error(error, "快照账面数"))?;
            lines.push(NewStockTakeLine {
                material_id,
                unit_id: material.unit_id,
                location_id,
                book_qty,
                unit_cost,
            });
        }
        // 同一物料同一库位重复给两次 → 直接拒绝（否则过账会双倍调平）
        let mut seen = std::collections::BTreeSet::new();
        for line in &lines {
            let key = (line.material_id, line.location_id);
            if !seen.insert(key) {
                return Err(bad_request(format!(
                    "抽盘明细里有重复行（物料 {} / 库位 {:?}）：同一桶只能盘一次",
                    line.material_id, line.location_id
                )));
            }
        }
    }
    if lines.len() > model::MAX_LINES {
        return Err(bad_request(format!(
            "盘点明细最多 {} 行（本次快照出 {} 行）：请改用抽盘分批盘",
            model::MAX_LINES,
            lines.len()
        )));
    }

    let new = NewStockTake {
        warehouse_id,
        take_date,
        take_type: &take_type,
        remark: Some(remark.as_str()),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match model::create(&tx, &new, &lines, op, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建盘点单"))
        }
    }
}

// ==================== 编辑（填实盘数） ====================

/// 编辑核心（**仅 DRAFT**）：改日期 / 备注 + 逐行写实盘数，然后重算单头合计。
pub async fn update_stock_take(
    id: Uuid,
    req: &UpdateStockTakeReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let remark = req.remark_text()?.map(str::to_string);
    let lines = req.parsed_lines()?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启编辑事务"))?;
    let outcome = async {
        let header = model::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定盘点单"))?
            .ok_or_else(|| not_found("盘点单"))?;
        if header.status != STATUS_DRAFT {
            return Err(conflict(format!(
                "盘点单 {} 当前是「{}」（{}）：只有草稿能改。已提交的单据要改请先撤回/驳回",
                header.take_no,
                status_label(&header.status),
                header.status
            )));
        }
        let take_date = req.take_date.unwrap_or(header.take_date);
        let remark_text = remark.clone().or(header.remark.clone());
        model::update_head(&tx, id, take_date, remark_text.as_deref(), op.id, now)
            .await
            .map_err(|error| write_error(error, "更新盘点单"))?;
        for (item_id, actual_qty) in &lines {
            let affected = model::set_actual_qty(&tx, id, *item_id, *actual_qty, now)
                .await
                .map_err(|error| write_error(error, "写实盘数"))?;
            if affected == 0 {
                return Err(bad_request(format!(
                    "明细 {item_id} 不属于盘点单 {}：请刷新后重试（明细不能在编辑里增删，换范围请新建）",
                    header.take_no
                )));
            }
        }
        model::refresh_totals(&tx, id, op.id, now)
            .await
            .map_err(|error| write_error(error, "重算盘点合计"))?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交编辑事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 删除 / 状态流转 ====================

/// 删除（**仅 DRAFT**，软删；顺带终止可能残留的审批实例）。
pub async fn delete_stock_take(
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
        let header = model::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定盘点单"))?
            .ok_or_else(|| not_found("盘点单"))?;
        if header.status != STATUS_DRAFT {
            return Err(conflict(format!(
                "盘点单 {} 当前是「{}」（{}）：只有草稿能删；已提交 / 已过账请走作废",
                header.take_no,
                status_label(&header.status),
                header.status
            )));
        }
        bridge::cancel_approval_of(&tx, id, &header.take_no, "盘点单被删除（草稿）")
            .await
            .map_err(|error| utils::db_error(error, "终止盘点审批实例"))?;
        let affected = model::soft_delete(&tx, id, op.id, now)
            .await
            .map_err(|error| write_error(error, "删除盘点单"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "盘点单 {} 的状态在本次删除期间被改动，请刷新后重试",
                header.take_no
            )));
        }
        Ok::<(), ApiError>(())
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

/// 状态流转（`PATCH /status` 与单测共用）。
///
/// | 目标 | 允许的起始态 | 动作 |
/// |---|---|---|
/// | `PENDING_POST` | DRAFT | 提交审批（起流程 / 未配流程走显式跳过） |
/// | `POSTED` | PENDING_POST | 过账（三本账 + 盘点差异凭证） |
/// | `DRAFT` | PENDING_POST | 撤回 / 驳回（终止在跑的审批） |
/// | `CANCELLED` | DRAFT / PENDING_POST / POSTED | 作废（已过账的走回滚 + 红冲） |
pub async fn change_status(
    id: Uuid,
    req: &StockTakeStatusReq,
    claims: Option<&TokenClaims>,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let target = req.target()?;
    let current = model::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单"))?
        .ok_or_else(|| not_found("盘点单"))?;
    if !model::LEGAL_EDGES
        .iter()
        .any(|(from, to)| *from == current.status.as_str() && *to == target.as_str())
    {
        return Err(conflict(format!(
            "盘点单 {}：当前「{}」（{}）不能转到「{}」（{}）。\
             合法路径：草稿→待过账（提交审批）、待过账→已过账（过账）、待过账→草稿（撤回/驳回）、\
             草稿/待过账/已过账→已作废",
            current.take_no,
            status_label(&current.status),
            current.status,
            status_label(&target),
            target
        )));
    }
    // ATT-06：盘点单的「确认」= 提交审批（DRAFT → PENDING_POST），此时盘点表 / 盈亏说明必传
    if target.as_str() == STATUS_PENDING_POST {
        attach_rule::ensure_attachments(AttachDocKind::StockTake, id, AttachAt::Confirm).await?;
    }
    match target.as_str() {
        STATUS_PENDING_POST => submit_for_approval(id, req, claims, op).await,
        STATUS_POSTED => post_stock_take(id, op, req.reason_text()?).await,
        STATUS_DRAFT => withdraw(id, req.reason_text()?, op).await,
        STATUS_CANCELLED => {
            let reason = req.reason_text()?;
            if current.status == STATUS_POSTED && reason.is_none() {
                return Err(bad_request(format!(
                    "作废已过账的盘点单 {} 必须写原因：原因要进状态日志与红字凭证摘要\
                     （审计要能回答「为什么这笔盘盈盘亏被撤销」）",
                    current.take_no
                )));
            }
            cancel_stock_take(id, reason.unwrap_or("盘点单作废"), op).await
        }
        other => Err(bad_request(format!("不支持的目标状态 {other}"))),
    }
}

/// `DRAFT → PENDING_POST`：提交审批（起流程；未配流程 → fail-closed / 显式跳过）。
async fn submit_for_approval(
    id: Uuid,
    req: &StockTakeStatusReq,
    claims: Option<&TokenClaims>,
    operator: state_machine::Operator,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启提交事务"))?;
    let mut effects = Vec::new();
    let outcome = async {
        let header = model::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定盘点单"))?
            .ok_or_else(|| not_found("盘点单"))?;
        if header.status != STATUS_DRAFT {
            return Err(conflict(format!(
                "盘点单 {} 当前是「{}」（{}），只有草稿可以提交审批",
                header.take_no,
                status_label(&header.status),
                header.status
            )));
        }
        let items = model::items_of(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "查询盘点明细"))?;
        if items.is_empty() {
            return Err(bad_request(format!(
                "盘点单 {} 没有任何明细，不能提交",
                header.take_no
            )));
        }
        // ① 制单 ≠ 盘点确认（P3-07，可配开关，默认关闭）
        //
        // 「确认」= 把草稿盘点单确认为「待过账」的这一步：实盘数与差异就此定稿，
        // 后面只剩审批与记账。制单人自己确认 = 既报数又核准数量，审计上不可追责。
        // 开关关闭时与改动前逐字一致（制单人可自行确认）。
        separation::guard(
            separation::Duty::Stocktake,
            header.create_by,
            operator.id,
            &format!("盘点单 {}", header.take_no),
        )
        .await?;
        // 幂等：同一单已有在跑的审批 → 409
        if flow_model::biz_instance_running(bridge::BIZ_TYPE, id)
            .await
            .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
        {
            return Err(conflict(format!(
                "盘点单 {} 已在审批中：请等审批结束，或从审批中心撤回后再提交",
                header.take_no
            )));
        }
        let flow = bridge::published_stock_take_flow()
            .await
            .map_err(|error| utils::db_error(error, "查询盘点审批流程"))?;

        // 审批闸：有流程必须走；没流程 → fail-closed，只有显式跳过才放行
        let mut skip_reason: Option<String> = None;
        match &flow {
            Some(flow) => {
                let gate = match claims {
                    Some(claims) => approval_gate::ApprovalGate::new(&req.gate, claims),
                    None => approval_gate::ApprovalGate::none(),
                };
                approval_gate::ensure_not_skipped(&flow.flow_code, &gate)?;
            }
            None => {
                let gate = match claims {
                    Some(claims) => approval_gate::ApprovalGate::new(&req.gate, claims),
                    None => approval_gate::ApprovalGate::none(),
                };
                let merged = approval_gate::authorize_missing_flow(
                    bridge::BIZ_TYPE,
                    bridge::FLOW_CODE,
                    &gate,
                )?;
                skip_reason = Some(merged);
            }
        }

        let mut instance_id: Option<Uuid> = None;
        if let Some(flow) = &flow {
            let Some(operator_id) = operator.id else {
                return Err(ApiError::new(
                    401,
                    error_code::AUTH_TOKEN_MISSING,
                    "提交审批需要登录用户（令牌里没有用户 id）",
                ));
            };
            let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
                .await
                .map_err(|error| utils::db_error(error, "查询发起人"))?
                .unwrap_or((String::new(), None));
            let started = Uuid::now_v7();
            let start = engine::StartReq {
                instance_id: started,
                def_id: flow.def_id,
                version_id: flow.version_id,
                version_no: flow.version_no,
                flow_code: flow.flow_code.clone(),
                flow_name: flow.flow_name.clone(),
                biz_type: flow.biz_type.clone(),
                biz_id: Some(id),
                biz_no: Some(header.take_no.clone()),
                title: format!("{} 盘点差异审批", header.take_no)
                    .chars()
                    .take(200)
                    .collect::<String>(),
                initiator_id: operator_id,
                initiator_name: Some(initiator_name),
                initiator_dept_id,
                form_data: stock_take_form_data(&header, &items),
            };
            let mut out = engine::start(&tx, &start).await.map_err(|error| {
                conflict(format!("发起盘点审批单失败（{}）：{error}", header.take_no))
            })?;
            if let Some(rejected) = out.rejected.take() {
                // FIX-1：原样透传（不要用 `ApiError::new(..)` 重建，那会丢 65018 的结构化载荷）
                return Err(rejected);
            }
            instance_id = Some(out.instance_id.unwrap_or(started));
            effects = out.effects.take();
            // 流程里没有审批节点（全被跳过 / 免审配置）→ 实例同事务就走完了：
            // 单据照样停在 PENDING_POST，等过账入口校验实例状态。
            if out.instance_status.eq_ignore_ascii_case("APPROVED") {
                tracing::info!(
                    take_id = %id,
                    take_no = %header.take_no,
                    "盘点审批流程没有需要人审的节点（自动通过）：单据进入待过账"
                );
            }
        }

        let reason = match skip_reason {
            None => Some("提交审批".to_string()),
            Some(text) => Some(text),
        };
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_TAKE,
            id,
            &header.take_no,
            &model::LEGAL_EDGES,
            STATUS_DRAFT,
            STATUS_PENDING_POST,
            operator.clone(),
            reason.as_deref(),
            None,
        )
        .await?;
        let affected = model::apply_status(
            &tx,
            id,
            STATUS_DRAFT,
            STATUS_PENDING_POST,
            reason.as_deref(),
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "提交盘点单待过账"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "盘点单 {} 的状态在本次提交期间被改动，请刷新后重试",
                header.take_no
            )));
        }
        if instance_id.is_some() {
            model::link_approval_instance(&tx, id, instance_id, now)
                .await
                .map_err(|error| write_error(error, "回写盘点审批实例"))?;
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交事务"))?;
            engine::flush_effects(effects).await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 盘点审批的**表单变量**。
///
/// `total_diff_amount` 必须是 JSON **数字**（不是字符串）：条件边比较走 `as_f64()`，
/// `"1234.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边
/// （把 <1 万的盘点静默升级成两级审批，或反过来把大额降级成单级 —— 审批最不该出错的地方）。
fn stock_take_form_data(header: &StockTakeRow, items: &[StockTakeItemRow]) -> serde_json::Value {
    let amount =
        header.total_gain_amount.max(Decimal::ZERO) + header.total_loss_amount.max(Decimal::ZERO);
    // 提交时差异金额还是 0（过账才算真金额），此时用「数量 × 账面成本」估一个
    // 金额给分级用：宁可估高走两级审批，也不要在提交时静默降级。
    let estimated: Decimal = items
        .iter()
        .map(|item| item.diff_qty.abs() * item.unit_cost)
        .sum();
    let basis = if amount > Decimal::ZERO {
        amount
    } else {
        estimated
    };
    let number = basis
        .round_dp(4)
        .to_string()
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "take_id": header.id.to_string(),
        "take_no": header.take_no,
        "warehouse_id": header.warehouse_id.to_string(),
        "take_type": header.take_type,
        "total_diff_amount": number,
        "total_diff_qty": header.total_diff_qty().to_string(),
        "line_count": items.len(),
    })
}

/// `PENDING_POST → DRAFT`：撤回 / 驳回（终止在跑的审批，账一分没动）。
async fn withdraw(
    id: Uuid,
    reason: Option<&str>,
    operator: state_machine::Operator,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启撤回事务"))?;
    let outcome = async {
        let header = model::lock(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定盘点单"))?
            .ok_or_else(|| not_found("盘点单"))?;
        if header.status != STATUS_PENDING_POST {
            return Err(conflict(format!(
                "盘点单 {} 当前是「{}」（{}），只有待过账可以撤回 / 驳回",
                header.take_no,
                status_label(&header.status),
                header.status
            )));
        }
        bridge::cancel_approval_of(
            &tx,
            id,
            &header.take_no,
            reason.unwrap_or("盘点单撤回（操作人主动把单据改回草稿）"),
        )
        .await
        .map_err(|error| utils::db_error(error, "终止盘点审批实例"))?;
        let text = reason.unwrap_or("撤回到草稿");
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_TAKE,
            id,
            &header.take_no,
            &model::LEGAL_EDGES,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            operator.clone(),
            Some(text),
            None,
        )
        .await?;
        let affected = model::apply_status(
            &tx,
            id,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            Some(text),
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "撤回到草稿"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "盘点单 {} 的状态在本次撤回期间被改动，请刷新后重试",
                header.take_no
            )));
        }
        model::link_approval_instance(&tx, id, None, now)
            .await
            .map_err(|error| write_error(error, "清空盘点审批实例"))?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交撤回事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 过账 ====================

/// 审批结论闸：挂了审批实例的单据，**实例必须是 APPROVED** 才允许过账。
///
/// 没有实例 = 提交时走的是「未配流程 + 显式跳过」路径（权限 + 理由已在提交时记录），
/// 这是唯一允许直接过账的形态；其余一律 409 点名「先过审批」。
async fn ensure_approved(header: &StockTakeRow) -> std::result::Result<(), ApiError> {
    let Some(instance_id) = header.approval_instance_id else {
        return Ok(());
    };
    let instance = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点审批实例"))?
        .ok_or_else(|| {
            conflict(format!(
                "盘点单 {} 挂着审批实例 {instance_id}，但实例已不存在：\
                 无法确认是否批过，拒绝过账。请撤回后重新提交审批",
                header.take_no
            ))
        })?;
    if instance.biz_type != bridge::BIZ_TYPE {
        return Err(conflict(format!(
            "盘点单 {} 挂的审批实例属于 {}（不是盘点审批）：数据异常，拒绝过账",
            header.take_no, instance.biz_type
        )));
    }
    if !instance.status.eq_ignore_ascii_case("APPROVED") {
        return Err(conflict(format!(
            "盘点单 {} 的审批尚未通过（当前 {}）：盘盈盘亏必须先审后记，\
             请等审批结束再过账",
            header.take_no, instance.status
        )));
    }
    Ok(())
}

/// 过账核心：`PENDING_POST → POSTED`（三本账 + 盘点差异凭证，同一事务）。
pub async fn post_stock_take(
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
    let outcome = post_tx(&tx, id, &op, reason, now).await;
    match outcome {
        Ok(change) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账事务"))?;
            // 提交后才跑通知 / 建任务（`Phase::AfterCommit`）：失败只记日志，不回滚已经落定的账
            stocktake_linkage::run_deferred_after_commit(change).await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn post_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: &state_machine::Operator,
    reason: Option<&str>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<stocktake_linkage::StockTakeStatusChange, ApiError> {
    // ① 锁单头
    let header = model::lock(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定盘点单"))?
        .ok_or_else(|| not_found("盘点单"))?;
    if header.status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "盘点单 {} 当前是「{}」（{}），不能过账：只有「待过账」可以过账（账只动一次）",
            header.take_no,
            status_label(&header.status),
            header.status
        )));
    }
    // ①b 期间闸：已结账的月份只读
    if let Some((period, status)) = model::period_closed(ex, header.take_date)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点日期所属会计期间"))?
        && status == crate::business::erp::voucher::PERIOD_CLOSED
    {
        return Err(bad_request(format!(
            "盘点单 {} 的盘点日期 {} 属于已结账的会计期间 {period}：已封账的月份只读，\
             请改到未结账的期间（或先反结账）再过账",
            header.take_no, header.take_date
        )));
    }
    // ①c 审批结论闸（审计铁律：没批过不许动账）
    ensure_approved(&header).await?;
    // ①d 制单 ≠ 过账（可配开关，默认关闭）
    separation::guard(
        separation::Duty::Post,
        header.create_by,
        operator.id,
        &format!("盘点单 {}", header.take_no),
    )
    .await?;
    // ①e 审批 ≠ 执行（P3-07，可配开关，默认关闭）
    //
    // 「执行」= 真的把盘盈盘亏记进三本账的这一步。审批人自己点过账 = 自己批、自己改账。
    // 审批人取的是这张盘点单审批实例里**所有投过同意票的人**（多级审批时只比最后一人
    // 会给「A 批、B 批、A 执行」留后门）。
    //
    // 没走审批（`approval_instance_id` 为空 = 提交时走的是「未配流程 + 显式跳过」）时
    // 没有对端可比 → 放行（跳过审批本身已经是一条带理由的审计留痕）。
    let approvers = separation::approvers_of(header.approval_instance_id).await;
    separation::guard_among(
        separation::Duty::ApproveExecute,
        &approvers,
        operator.id,
        &format!("盘点单 {}", header.take_no),
    )
    .await?;

    let items = model::items_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "盘点单 {} 没有任何明细，不能过账",
            header.take_no
        )));
    }

    // ② 状态机唯一入口 + CAS
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_TAKE,
        id,
        &header.take_no,
        &model::LEGAL_EDGES,
        STATUS_PENDING_POST,
        STATUS_POSTED,
        operator.clone(),
        reason,
        None,
    )
    .await?;
    let affected = model::apply_status(
        ex,
        id,
        STATUS_PENDING_POST,
        STATUS_POSTED,
        reason,
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "盘点单过账改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "盘点单 {} 的状态在本次过账期间被改动（期望 {}），请刷新后重试",
            header.take_no, STATUS_PENDING_POST
        )));
    }

    // ③ 逐行：差异正负分别走入库 / 出库方向的三本账
    let policy = model::warehouse_policy(ex, header.warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库负库存策略"))?
        .ok_or_else(|| reference_missing("盘点仓库不存在或已删除"))?;
    let mut applied: Vec<DiffApplied> = Vec::new();
    for item in &items {
        let diff = item.diff_qty.round_dp(4);
        if diff == Decimal::ZERO {
            applied.push(DiffApplied {
                item_id: item.id,
                diff_qty: Decimal::ZERO,
                amount: Decimal::ZERO,
            });
            continue;
        }
        let amount = if diff > Decimal::ZERO {
            model::apply_gain(ex, &header, item, operator.id, now)
                .await
                .map_err(|error| write_error(error, "过账：盘盈入账"))?
        } else {
            model::apply_loss(ex, &header, item, &policy, operator.id, now)
                .await
                .map_err(|error| write_error(error, "过账：盘亏出账"))?
        };
        model::set_item_amount(ex, item.id, amount, now)
            .await
            .map_err(|error| write_error(error, "过账：回写差异金额"))?;
        applied.push(DiffApplied {
            item_id: item.id,
            diff_qty: diff,
            amount,
        });
    }

    // ④ 单头差异合计
    let gain_qty: Decimal = applied
        .iter()
        .filter(|row| row.diff_qty > Decimal::ZERO)
        .map(|row| row.diff_qty)
        .sum();
    let loss_qty: Decimal = applied
        .iter()
        .filter(|row| row.diff_qty < Decimal::ZERO)
        .map(|row| -row.diff_qty)
        .sum();
    let gain_amount: Decimal = applied
        .iter()
        .filter(|row| row.diff_qty > Decimal::ZERO)
        .map(|row| row.amount)
        .sum();
    let loss_amount: Decimal = applied
        .iter()
        .filter(|row| row.diff_qty < Decimal::ZERO)
        .map(|row| row.amount)
        .sum();
    model::set_diff_totals(ex, id, gain_qty, loss_qty, gain_amount, loss_amount, now)
        .await
        .map_err(|error| write_error(error, "过账：回写差异合计"))?;

    // ⑤ 盘点差异凭证（借 1405 / 贷 1901 与借 1901 / 贷 1405 成对出现）
    let pairs: Vec<VoucherPair> = items
        .iter()
        .zip(applied.iter())
        .filter(|(_, result)| result.diff_qty != Decimal::ZERO)
        .map(|(item, result)| VoucherPair {
            line_no: item.line_no,
            material_id: item.material_id,
            amount: result.amount,
            gain: result.diff_qty > Decimal::ZERO,
        })
        .collect();
    let refreshed = StockTakeRow {
        total_gain_amount: gain_amount.round_dp(4),
        total_loss_amount: loss_amount.round_dp(4),
        ..header.clone()
    };
    let voucher_id = model::create_voucher(ex, &refreshed, &pairs, operator.id, now)
        .await
        .map_err(|error| write_error(error, "过账：生成盘点差异凭证"))?;
    if voucher_id.is_some() {
        model::link_voucher(ex, id, voucher_id, false, now)
            .await
            .map_err(|error| write_error(error, "过账：回写凭证引用"))?;
    }
    tracing::info!(
        take_id = %id,
        take_no = %header.take_no,
        gain_qty = %gain_qty,
        loss_qty = %loss_qty,
        gain_amount = %gain_amount,
        loss_amount = %loss_amount,
        voucher = voucher_id.map(|v| v.to_string()).unwrap_or_default(),
        "盘点单过账完成：三本账已按差异动过，盘点差异凭证已生成"
    );

    // ⑥ 跨单据联动（通用引擎，LNK-04）：触发点 = `STOCK_TAKE/POSTED`。
    //
    // ③ 已经把盘盈盘亏写进成本账 / 库位账 / 流水，⑤ 已经出了盘点差异凭证，
    // 所以这里必须把 `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `GENERATE_VOUCHER`
    // 交给专用路径（`spec::STOCKTAKE_EXCLUDED_ACTIONS`），否则同一笔差异落两次账。
    // 其余事务内动作照跑；`deferred`（发通知 / 建任务）由 `post_stock_take` 在
    // commit 之后交给 `run_deferred_after_commit`。
    //
    // 读规则失败 = 空规则集 = 行为与配置前逐字相同（`model::rules_for` fail-safe）；
    // 关键动作失败 → `Err` → 调用方整笔回滚（与本函数其它步骤同一口径）。
    let change = stocktake_linkage::run_in_tx(ex, &header, STATUS_POSTED, operator.id).await?;
    Ok(change)
}

// ==================== 作废 ====================

/// 作废：`DRAFT | PENDING_POST → CANCELLED`（账没动过）与
/// `POSTED → CANCELLED`（三本账回滚 + 红字凭证）两条路。
pub async fn cancel_stock_take(
    id: Uuid,
    reason: &str,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废事务"))?;
    let outcome = cancel_tx(&tx, id, reason, &op, now).await;
    match outcome {
        Ok(change) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废事务"))?;
            // 提交后才跑通知 / 建任务：失败只记日志，不回滚已经回滚过的账
            stocktake_linkage::run_deferred_after_commit(change).await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn cancel_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: &str,
    operator: &state_machine::Operator,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<stocktake_linkage::StockTakeStatusChange, ApiError> {
    let header = model::lock(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定盘点单"))?
        .ok_or_else(|| not_found("盘点单"))?;
    let from_status = header.status.clone();
    if from_status == STATUS_CANCELLED {
        return Err(conflict(format!(
            "盘点单 {} 已经作废过了（作废不重复执行，账只回滚一次）",
            header.take_no
        )));
    }
    // 待过账作废：先把在跑的审批终止，别留孤儿实例
    if from_status == STATUS_PENDING_POST {
        bridge::cancel_approval_of(ex, id, &header.take_no, reason)
            .await
            .map_err(|error| utils::db_error(error, "终止盘点审批实例"))?;
    }
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_TAKE,
        id,
        &header.take_no,
        &model::LEGAL_EDGES,
        &from_status,
        STATUS_CANCELLED,
        operator.clone(),
        Some(reason),
        None,
    )
    .await?;
    let affected = model::apply_status(
        ex,
        id,
        &from_status,
        STATUS_CANCELLED,
        Some(reason),
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "盘点单作废改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "盘点单 {} 的状态在本次作废期间被改动（期望 {}），请刷新后重试",
            header.take_no, from_status
        )));
    }
    // ③ 跨单据联动（通用引擎，LNK-04）：触发点 = `STOCK_TAKE/CANCELLED`。
    //
    // 三个来源态（DRAFT / PENDING_POST / POSTED）都走同一处：状态已经 CAS 落定，
    // 规则读的是「作废」这件事，与账有没有动过无关。
    // 盘点差异的库存回滚（下面 ④）与红冲凭证都在这个事务里由专用路径做，
    // 所以 `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `GENERATE_VOUCHER` 必须排除
    // （`spec::STOCKTAKE_EXCLUDED_ACTIONS`）；in-tx 其余动作都是占位动作，
    // 放在回滚之前跑不会读到「还没回滚」的中间态。
    let change = stocktake_linkage::run_in_tx(ex, &header, STATUS_CANCELLED, operator.id).await?;

    // 草稿 / 待过账：从未动过账，到此为止
    if from_status != STATUS_POSTED {
        tracing::info!(
            take_id = %id,
            take_no = %header.take_no,
            from = %from_status,
            "盘点单作废（未过账，三本账未动）"
        );
        return Ok(change);
    }

    // 已过账 → 三本账原路回滚（数量与金额都按过账时写下的事实）
    let items = model::items_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点明细"))?;
    for item in &items {
        let diff = item.diff_qty.round_dp(4);
        if diff == Decimal::ZERO {
            continue;
        }
        if diff > Decimal::ZERO {
            model::reverse_gain(ex, &header, item, operator.id, now)
                .await
                .map_err(|error| write_error(error, "作废：回滚盘盈行"))?;
        } else {
            model::reverse_loss(ex, &header, item, operator.id, now)
                .await
                .map_err(|error| write_error(error, "作废：回滚盘亏行"))?;
        }
    }

    // 红字凭证（找不到原凭证 = 当时零成本跳过，不阻断作废）
    let red = model::reverse_voucher(ex, &header, Some(reason), operator.id, now)
        .await
        .map_err(|error| write_error(error, "作废：红冲盘点差异凭证"))?;
    if red.is_some() {
        model::link_voucher(ex, id, red, true, now)
            .await
            .map_err(|error| write_error(error, "作废：回写红字凭证引用"))?;
    }
    tracing::info!(
        take_id = %id,
        take_no = %header.take_no,
        red_voucher = red.map(|v| v.to_string()).unwrap_or_default(),
        "盘点单作废：三本账已回滚，原凭证已红冲"
    );
    Ok(change)
}

// ==================== 详情 / 分页 ====================

/// 给单头（列表 / 详情都走它）回填凭证号 `PZ-…` / `PZR-…`。
///
/// 为什么不在 `StockTakeRes::of` 里查：那是个纯函数（单测用假行直接调），带 IO 会让
/// 断言全得挂库。凭证号是**展示字段**，只在这一层补，序列化出来和 `voucher_id` 成对。
async fn fill_voucher_nos(res: &mut [StockTakeRes]) -> std::result::Result<(), ApiError> {
    let mut ids: Vec<Uuid> = Vec::with_capacity(res.len() * 2);
    for row in res.iter() {
        for raw in [row.voucher_id.as_deref(), row.reverse_voucher_id.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Ok(id) = Uuid::parse_str(raw) {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let map = model::voucher_no_map(db(), &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点凭证号"))?;
    let lookup = |raw: Option<&str>| -> Option<String> {
        raw.and_then(|text| Uuid::parse_str(text).ok())
            .and_then(|id| map.get(&id).cloned())
    };
    for row in res.iter_mut() {
        row.voucher_no = lookup(row.voucher_id.as_deref());
        row.reverse_voucher_no = lookup(row.reverse_voucher_id.as_deref());
    }
    Ok(())
}

/// 详情核心（HTTP 与单测共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<StockTakeDetailRes, ApiError> {
    let row = model::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单"))?
        .ok_or_else(|| not_found("盘点单"))?;
    let items = model::items_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点明细"))?;
    let mut pairs = Vec::with_capacity(items.len());
    for item in items {
        let material = stockout_model::material_info(db(), item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?;
        pairs.push((item, material));
    }
    let edges = state_machine::effective_edges(db(), state_machine::Entity::StockTake).await;
    let mut detail = StockTakeDetailRes::of(&row, &pairs, &edges);
    fill_voucher_nos(std::slice::from_mut(&mut detail.header)).await?;
    Ok(detail)
}

/// 分页核心。
pub async fn page_of(q: &StockTakePageQuery) -> std::result::Result<StockTakePageRes, ApiError> {
    let filter = q.to_filter()?;
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单列表"))?;
    let mut list: Vec<StockTakeRes> = rows.iter().map(StockTakeRes::of).collect();
    fill_voucher_nos(&mut list).await?;
    Ok(StockTakePageRes {
        list,
        total,
        page: filter.page,
        page_size: filter.page_size,
    })
}

/// 列表行的分页结果（供批量 / 导出复用）。
pub fn list_res(rows: &[StockTakeRow]) -> Vec<StockTakeRes> {
    rows.iter().map(StockTakeRes::of).collect()
}

// ==================== HTTP 入口 ====================

/// 新建盘点单（`status = DRAFT`，单号 `ST-yyyyMMdd-000001`）。
///
/// # 异常情况
///
/// * 401 未登录；403 缺 `business:erp:stock-take:create`；
/// * 400（`10001` / `66002`）：仓库不存在 / 抽盘没给物料 / 全盘给了 items /
///   该仓没有库存行 / 实盘数为负 / 明细超上限；
/// * 409（`66001`）：单号或行号唯一键冲突（并发取号）；500：数据库异常。
#[nik::permission(
    path = "/business/erp/stock-take",
    method = "POST",
    permission = "business:erp:stock-take:create"
)]
#[nik::post("/business/erp/stock-take")]
pub async fn stock_take_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateStockTakeReq>,
) -> ResResult<StockTakeEnvelope> {
    let id = create_stock_take(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 编辑盘点单（**仅草稿**）：改日期 / 备注 + 逐行填实盘数。
#[nik::permission(
    path = "/business/erp/stock-take/{id}",
    method = "PUT",
    permission = "business:erp:stock-take:update"
)]
#[nik::put("/business/erp/stock-take/{id}")]
pub async fn stock_take_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateStockTakeReq>,
) -> ResResult<StockTakeEnvelope> {
    let id = Uuid::parse_str(id.trim())
        .map_err(|err| bad_request(format!("id 不是合法 UUID：{err}")))?;
    update_stock_take(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 删除盘点单（**仅草稿**，软删）。
#[nik::permission(
    path = "/business/erp/stock-take/{id}",
    method = "DELETE",
    permission = "business:erp:stock-take:delete"
)]
#[nik::delete("/business/erp/stock-take/{id}")]
pub async fn stock_take_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = Uuid::parse_str(id.trim())
        .map_err(|err| bad_request(format!("id 不是合法 UUID：{err}")))?;
    delete_stock_take(id, &claims).await?;
    Ok(Res::ok(()))
}

/// 状态流转：提交审批 / 过账 / 撤回 / 作废（同一入口，边表决定合法性）。
#[nik::permission(
    path = "/business/erp/stock-take/{id}/status",
    method = "PATCH",
    permission = "business:erp:stock-take:status"
)]
#[nik::patch("/business/erp/stock-take/{id}/status")]
pub async fn stock_take_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StockTakeStatusReq>,
) -> ResResult<StockTakeEnvelope> {
    let id = Uuid::parse_str(id.trim())
        .map_err(|err| bad_request(format!("id 不是合法 UUID：{err}")))?;
    change_status(id, &req, Some(&claims), &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 盘点单分页。
#[nik::permission(
    path = "/business/erp/stock-take/page",
    method = "GET",
    permission = "business:erp:stock-take:list"
)]
#[nik::get("/business/erp/stock-take/page")]
pub async fn stock_take_page(Query(q): Query<StockTakePageQuery>) -> ResResult<StockTakePageRes> {
    Ok(Res::ok(page_of(&q).await?))
}

/// 盘点单详情（含明细与差异）。
#[nik::permission(
    path = "/business/erp/stock-take/{id}",
    method = "GET",
    permission = "business:erp:stock-take:detail"
)]
#[nik::get("/business/erp/stock-take/{id}")]
pub async fn stock_take_detail(Path(id): Path<String>) -> ResResult<StockTakeDetailRes> {
    let id = Uuid::parse_str(id.trim())
        .map_err(|err| bad_request(format!("id 不是合法 UUID：{err}")))?;
    Ok(Res::ok(detail_of(id).await?))
}
