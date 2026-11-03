//! 联动作废编排（ARCH-3 §6 **R-P1-5** · 断链 DC-24）：「一键把三遍手工活做完，但先把影响面摊开」。
//!
//! # 为什么要有这个模块
//!
//! 一张「已收货」的采购单，账其实是**平**的（D1/E1 巡检验得过），但你要把它废掉，
//! 得自己跑三个页面：作废收货单（回滚库存三本账 + 退序列号）→ 红冲进项凭证 →
//! 作废应付单，最后才轮到采购单本身。少跑一步就是一笔孤儿账，而采购单的状态机
//! 甚至不允许 `RECEIVED → CANCELLED`（13 条边里没有这一条），于是「系统直接拒绝」。
//!
//! 本模块给两个接口：
//!
//! | 接口 | 作用 |
//! |---|---|
//! | `GET  /business/erp/{doc_type}/{id}/cancel-impact`  | **影响分析**：作废这张单会波及哪些下游单据、每张怎么处理、有没有拦路虎 |
//! | `POST /business/erp/{doc_type}/{id}/cancel-cascade` | **联动作废**：一个事务、固定顺序、任一步失败整笔回滚 |
//!
//! # 为什么不是「静默自动级联」（卡面硬红线）
//!
//! 前端必须**先**调 `cancel-impact` 把影响面摆给操作员看（弹窗里逐条列单号 + 金额 +
//! 处理方式），操作员点「一键按顺序处理」之后才调 `cancel-cascade`。
//! 后端不做「你点作废我就顺手把下游全改了」——那是替用户做主，账务上不允许。
//!
//! # 执行顺序（一个事务，`db().begin()` 到 `commit()`）
//!
//! ```text
//! ① 作废收货单（POSTED / PENDING_CANCEL / PENDING_POST）
//!    └ 这一步内部就是 P9/P16 的既有作废链路（`stockin_handler_cancel::cancel_in_tx`），
//!      顺序不变：退序列号 → 成本账 → 库位账 → 反向流水 → 红字凭证 → 应付作废 →
//!      回退采购已收数量 → 重算采购单状态。**一行回滚逻辑都没有重写。**
//! ② 作废采购单（状态机 → CANCELLED，在跑的审批单先终止）
//! ```
//!
//! 凭证红冲与应付作废**不需要单独两步**：它们本来就长在收货单作废的同一个事务里
//! （`cancel_in_tx` 的第 ④⑤ 步）。影响分析里仍然把这两张单**逐条列出来**
//! （`performed_in_step = 收货单那一步`），因为操作员要知道「我废这张单会红冲掉哪张凭证」。
//!
//! 为什么不把红冲提到前面单独执行：`voucher::reverse_for_stock_in` 需要收货单行
//! 与它过账时写下的金额口径，脱离 `cancel_in_tx` 就要另复制一份取值逻辑 ——
//! 那正是「两个真相源」的老坑（skill §6.36）。卡的顺序描述的是**净效果**，
//! 一个事务里的净效果与「先红冲再作废」完全等价。
//!
//! # 支持的 `doc_type`
//!
//! | `doc_type` | 下游 | 说明 |
//! |---|---|---|
//! | `purchase-order` | 收货单 → 凭证 / 应付 | 卡的正面场景 |
//! | `stock-in` | 凭证 / 应付 | 只是把「作废收货单」这件事的影响面摊开 |
//! | `payable` | 凭证 | 未付款的应付单作废（进项凭证红冲） |
//! | `transfer` | 无 | **不支持**：调拨单作废不跨单据联动，沿用 `PATCH /transfer/{id}/status` |
//!
//! # 权限与审计
//!
//! * 读影响面：`business:erp:doc:cancel-impact`
//! * 执行级联：`business:erp:doc:cancel-cascade`
//! * 每一次级联都在 `biz_status_log` 里留下**每一步**的痕迹（由各单据自己的
//!   `state_machine::transition` 写），reason 统一带「联动作废：」前缀，
//!   事后能一眼看出这三张单是被同一次操作废掉的。
//!
//! # 审批流与级联的关系（重要，别想当然）
//!
//! 单张单据的作废在生产上走 `stock_in_cancel` / `payable_cancel` 审批（P16 / P-FLOW-3B）。
//! 联动作废**不再逐张走审批**：一个事务里开不出三张审批单，也等不到三个人批。
//! 取而代之的是：影响面弹窗本身就是「这一次操作要动哪些账」的显式确认，
//! 权限码 `business:erp:doc:cancel-cascade` 只发给有权一次改多张账的角色（默认超管）；
//! 各单据的 `state_machine` 仍然执行，`biz_status_log` 仍然逐条留痕。
//! 想恢复「逐张复核」的客户，不要给这个权限码即可。

use axum::{Extension, Json};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::payable;
use crate::business::erp::payable_payment;
use crate::business::erp::purchase_bridge;
use crate::business::erp::purchase_model;
use crate::business::erp::stockin_cancel;
use crate::business::erp::stockin_dto;
use crate::business::erp::stockin_handler_cancel;
use crate::business::erp::stockin_model;
use crate::business::erp::stockin_query;
use crate::business::erp::voucher;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 采购订单（`PO-yyyyMMdd-000001`）。
pub const DOC_TYPE_PURCHASE_ORDER: &str = "purchase-order";
/// 收货入库单（`IN-yyyyMMdd-000001`）。
pub const DOC_TYPE_STOCK_IN: &str = "stock-in";
/// 应付单（`AP-yyyyMMdd-000001`）。
pub const DOC_TYPE_PAYABLE: &str = "payable";
/// 库存调拨单（`TK-yyyyMMdd-000001`）—— 不支持级联（见模块文档）。
pub const DOC_TYPE_TRANSFER: &str = "transfer";

/// 处理方式：本模块真的会执行。
pub const ACTION_EXECUTE: &str = "EXECUTE";
/// 处理方式：已经处理过（幂等跳过）。
pub const ACTION_SKIP: &str = "SKIP";
/// 处理方式：拦路虎（存在它就不许级联，`can_cancel = false`）。
pub const ACTION_BLOCKED: &str = "BLOCKED";

/// 步骤执行结果：做完了。
pub const OUTCOME_DONE: &str = "DONE";
/// 步骤执行结果：本来就是终态（幂等跳过）。
pub const OUTCOME_SKIPPED: &str = "SKIPPED";

/// 级联理由的最小长度（会计单据作废必须能追溯为什么）。
pub const MIN_REASON_LEN: usize = 4;

/// 级联理由的长度上限（与收货单作废同一口径，复用它的常量避免两处漂移）。
pub const MAX_REASON_LEN: usize = stockin_dto::MAX_REASON_LEN;

// ==================== 出参 / 入参 ====================

/// 影响面里的一行（一张会被波及的单据 / 一个会被执行的动作）。
#[derive(Debug, Clone, Serialize)]
pub struct CancelImpactItem {
    /// 执行序号（1 起）。同号的几条是**同一个事务步骤**里一起做的
    /// （例如收货单作废会顺手红冲凭证、作废应付）。
    pub exec_step: u32,
    /// 单据类型（`purchase-order` / `stock-in` / `voucher` / `payable`）。
    pub doc_type: String,
    /// 单据类型中文名（弹窗直接用，前端不维护映射表）。
    pub doc_type_label: String,
    /// 单据 id（凭证 / 应付是它们自己的 id，可点进去看）。
    pub doc_id: String,
    /// 单号（`PO-…` / `IN-…` / `PZ-…` / `AP-…`）。
    pub doc_no: String,
    /// 单据当前状态。
    pub doc_status: String,
    /// 状态中文名。
    pub doc_status_label: String,
    /// 金额（含税 / 原凭证借方合计；无金额的动作为 0）。
    pub amount: Decimal,
    /// 处理方式：`EXECUTE` / `SKIP` / `BLOCKED`。
    pub action: String,
    /// 处理方式的一句话说明（弹窗的一级文案）。
    pub action_label: String,
    /// 补充说明（弹窗二级文案：为什么跳过 / 为什么拦）。
    pub detail: String,
}

/// 影响分析出参（`GET …/cancel-impact`）。
#[derive(Debug, Clone, Serialize)]
pub struct CancelImpactRes {
    /// 被作废的单据类型。
    pub doc_type: String,
    /// 被作废的单据 id。
    pub doc_id: String,
    /// 被作废的单号。
    pub doc_no: String,
    /// 被作废单据的当前状态。
    pub doc_status: String,
    /// 状态中文名。
    pub doc_status_label: String,
    /// 被作废单据的金额（采购单 / 收货单 = 价税合计；应付 = 应付金额；调拨单 = 0）。
    pub amount: Decimal,
    /// **能不能一键处理**：`false` 时前端只允许「取消」，并把 `blockers` 显示出来。
    pub can_cancel: bool,
    /// 弹窗标题用的一句话：「作废 PO-… 将影响以下 3 张单据」。
    pub summary: String,
    /// 受影响单据（按执行顺序）。
    pub steps: Vec<CancelImpactItem>,
    /// 拦路虎（空 = 可以一键处理）。每条都是一句可执行的话。
    pub blockers: Vec<String>,
    /// 固定提示（前端直接显示，别自己拼）。
    pub note: String,
}

/// 联动作废入参。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CancelCascadeReq {
    /// 作废原因（必填，≥ [`MIN_REASON_LEN`] 字）—— 会计上作废必须留痕。
    pub reason: Option<String>,
}

/// 联动作废里一步的执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct CancelCascadeStep {
    /// 执行序号（1 起）。
    pub seq: u32,
    /// 单据类型。
    pub doc_type: String,
    /// 单号。
    pub doc_no: String,
    /// 结果：`DONE` / `SKIPPED`。
    pub outcome: String,
    /// 人话说明（「已作废（三本账回滚 + 凭证红冲 + 应付作废）」等）。
    pub message: String,
}

/// 联动作废出参（`POST …/cancel-cascade`）。
#[derive(Debug, Clone, Serialize)]
pub struct CancelCascadeRes {
    /// 被作废的单据类型。
    pub doc_type: String,
    /// 被作废的单号。
    pub doc_no: String,
    /// 逐步执行结果（顺序 = 真实执行顺序）。
    pub steps: Vec<CancelCascadeStep>,
    /// 一句话总结（前端 toast 直接用）。
    pub message: String,
    /// 事务边界说明（审计：这几次改动是同生共死的）。
    pub transaction: String,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：单据不存在。
fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 路径里的 UUID：非法 → `400 + 66006`。
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

/// 操作人。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// `doc_type` 归一化：接受驼峰 / 下划线 / 简写，统一成模块常量。
///
/// 前端路由里写 `purchase-order` 最自然，但历史调用方写过 `purchase_order`，
/// 两形都收，免得 404 之后还要猜。
fn normalize_doc_type(raw: &str) -> std::result::Result<&'static str, ApiError> {
    let text = raw.trim().to_ascii_lowercase().replace('_', "-");
    match text.as_str() {
        "purchase-order" | "purchaseorder" | "po" => Ok(DOC_TYPE_PURCHASE_ORDER),
        "stock-in" | "stockin" | "in" => Ok(DOC_TYPE_STOCK_IN),
        "payable" | "ap" => Ok(DOC_TYPE_PAYABLE),
        "transfer" | "tk" => Ok(DOC_TYPE_TRANSFER),
        other => Err(bad_request(format!(
            "doc_type 不支持：{other}（只支持 purchase-order / stock-in / payable；\
             transfer 的作废不跨单据联动，请用 PATCH /business/erp/transfer/{{id}}/status）"
        ))),
    }
}

/// 中文类型名。
fn doc_type_label(doc_type: &str) -> &'static str {
    match doc_type {
        DOC_TYPE_PURCHASE_ORDER => "采购订单",
        DOC_TYPE_STOCK_IN => "收货入库单",
        DOC_TYPE_PAYABLE => "应付单",
        DOC_TYPE_TRANSFER => "库存调拨单",
        "voucher" => "凭证",
        _ => "单据",
    }
}

/// 采购单状态中文名。
fn purchase_status_label(status: &str) -> String {
    match status {
        purchase_model::STATUS_DRAFT => "草稿",
        purchase_model::STATUS_SUBMITTED => "已提交",
        purchase_model::STATUS_APPROVED => "已审批",
        purchase_model::STATUS_PARTIAL_RECEIVED => "部分收货",
        purchase_model::STATUS_RECEIVED => "已收货",
        purchase_model::STATUS_CLOSED => "已关闭",
        purchase_model::STATUS_CANCELLED => "已作废",
        other => other,
    }
    .to_string()
}

/// 收货单状态中文名。
fn stock_in_status_label(status: &str) -> String {
    match status {
        stockin_model::STATUS_DRAFT => "草稿",
        stockin_model::STATUS_POSTED => "已过账",
        stockin_model::STATUS_PENDING_POST => "过账审批中",
        stockin_model::STATUS_PENDING_CANCEL => "作废审批中",
        stockin_model::STATUS_CANCELLED => "已作废",
        other => other,
    }
    .to_string()
}

/// 应付单状态中文名。
fn payable_status_label(status: &str) -> String {
    match status {
        payable::STATUS_OPEN => "未付",
        payable::STATUS_PARTIAL_PAID => "部分付款",
        payable::STATUS_SETTLED => "已结清",
        payable::STATUS_PENDING_CANCEL => "作废审批中",
        payable::STATUS_CANCELLED => "已作废",
        other => other,
    }
    .to_string()
}

/// 把库层哨兵前缀翻成人话（`INV_SHORTFALL|…` 这类前缀只给代码分支用，别给用户看）。
fn readable(message: &str) -> String {
    let mut text = message.to_string();
    for prefix in [
        stockin_cancel::SHORTFALL_PREFIX,
        stockin_cancel::MISSING_TXN_PREFIX,
        stockin_cancel::SERIAL_SHIPPED_PREFIX,
        stockin_cancel::PO_RACE_PREFIX,
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_string();
            break;
        }
    }
    text
}

// ==================== 读：下游单据 ====================

/// 影响分析用的收货单摘要。
#[derive(Debug, Clone)]
struct StockInBrief {
    id: Uuid,
    in_no: String,
    status: String,
    warehouse_id: Uuid,
    total_with_tax: Decimal,
}

/// 一张下游凭证的摘要。
#[derive(Debug, Clone)]
struct VoucherBrief {
    id: Uuid,
    voucher_no: String,
    amount: Decimal,
    reversed: bool,
}

/// 一张下游应付单的摘要。
#[derive(Debug, Clone)]
struct PayableBrief {
    id: Uuid,
    payable_no: String,
    status: String,
    amount: Decimal,
    paid_amount: Decimal,
}

/// 某采购单的下游收货单（未删除，按单号升序 —— 顺序稳定，弹窗不会每次不一样）。
async fn stock_ins_of_order(order_id: Uuid) -> std::result::Result<Vec<StockInBrief>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, in_no, status, warehouse_id, total_with_tax \
               FROM biz_stock_in \
              WHERE source_order_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY in_no",
            vec![Value::from(&order_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询采购单的下游收货单"))?;
    rows.iter()
        .map(|row| {
            Ok(StockInBrief {
                id: row.get_typed("id")?,
                in_no: row.get_typed("in_no")?,
                status: row.get_typed("status")?,
                warehouse_id: row.get_typed("warehouse_id")?,
                total_with_tax: row.get_typed("total_with_tax")?,
            })
        })
        .collect::<std::result::Result<Vec<_>, DbError>>()
        .map_err(|error| utils::db_error(error, "读取下游收货单"))
}

/// 收货单作废会红冲的那张凭证（没有 → `None`）。
async fn voucher_of(stock_in_id: Uuid) -> std::result::Result<Option<VoucherBrief>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, voucher_no, total_debit, reversed_by \
               FROM biz_voucher \
              WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid AND is_red = 0 \
              ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询收货单的进项凭证"))?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(VoucherBrief {
            id: row.get_typed("id")?,
            voucher_no: row.get_typed("voucher_no")?,
            amount: row.get_typed("total_debit")?,
            reversed: row
                .get_typed::<Option<Uuid>>("reversed_by")
                .ok()
                .flatten()
                .is_some(),
        })),
    }
}

/// 某张凭证的摘要（按 id 取，应付单作废用）。
async fn voucher_by_id(voucher_id: Uuid) -> std::result::Result<Option<VoucherBrief>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, voucher_no, total_debit, reversed_by FROM biz_voucher WHERE id = ?::uuid",
            vec![Value::from(&voucher_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(VoucherBrief {
            id: row.get_typed("id")?,
            voucher_no: row.get_typed("voucher_no")?,
            amount: row.get_typed("total_debit")?,
            reversed: row
                .get_typed::<Option<Uuid>>("reversed_by")
                .ok()
                .flatten()
                .is_some(),
        })),
    }
}

/// 收货单联动的应付单（未删除；理论上最多一张，取最新）。
async fn payable_of_stock_in(
    stock_in_id: Uuid,
) -> std::result::Result<Option<PayableBrief>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, payable_no, status, amount, paid_amount \
               FROM biz_payable \
              WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询收货单的应付单"))?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PayableBrief {
            id: row.get_typed("id")?,
            payable_no: row.get_typed("payable_no")?,
            status: row.get_typed("status")?,
            amount: row.get_typed("amount")?,
            paid_amount: row.get_typed("paid_amount")?,
        })),
    }
}

// ==================== 影响分析 ====================

/// 影响分析（`GET /business/erp/{doc_type}/{id}/cancel-impact`）。
///
/// 只读：**不锁行、不开事务、不改任何一张表**，可以随便点。
/// 唯一的重活是收货单的「可行性预检」（[`stockin_model::ensure_cancellable`]：
/// 库存够不够退 / 序列号有没有已经出库），它也是只读的。
pub async fn analyze(doc_type: &str, id: Uuid) -> std::result::Result<CancelImpactRes, ApiError> {
    match normalize_doc_type(doc_type)? {
        DOC_TYPE_PURCHASE_ORDER => analyze_purchase_order(id).await,
        DOC_TYPE_STOCK_IN => analyze_stock_in(id).await,
        DOC_TYPE_PAYABLE => analyze_payable(id).await,
        other => Err(bad_request(format!(
            "doc_type={other} 不支持联动作废影响分析：{}",
            transfer_hint()
        ))),
    }
}

/// 调拨单的提示语（两个入口共用，避免两处文案漂移）。
fn transfer_hint() -> &'static str {
    "调拨单的作废只在 DRAFT / PENDING 两个状态下有意义（货还没动），不涉及跨单据联动，\
     请直接用 PATCH /business/erp/transfer/{id}/status"
}

/// 采购单的影响分析。
async fn analyze_purchase_order(id: Uuid) -> std::result::Result<CancelImpactRes, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let briefs = stock_ins_of_order(id).await?;

    let mut steps: Vec<CancelImpactItem> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut exec_step: u32 = 0;

    for brief in &briefs {
        // 已作废的收货单：不占执行步骤，只作为「已经处理过」列出来
        if brief.status == stockin_model::STATUS_CANCELLED {
            steps.push(CancelImpactItem {
                exec_step,
                doc_type: DOC_TYPE_STOCK_IN.to_string(),
                doc_type_label: doc_type_label(DOC_TYPE_STOCK_IN).to_string(),
                doc_id: brief.id.to_string(),
                doc_no: brief.in_no.clone(),
                doc_status: brief.status.clone(),
                doc_status_label: stock_in_status_label(&brief.status),
                amount: brief.total_with_tax,
                action: ACTION_SKIP.to_string(),
                action_label: "已作废（跳过）".to_string(),
                detail: "这张收货单早前已经作废，本次不再动它".to_string(),
            });
            continue;
        }
        exec_step += 1;
        let step = exec_step;
        let mut detail = String::from("三本账（成本 / 库位 / 流水）反向冲减 + 序列号退回");
        if brief.status == stockin_model::STATUS_DRAFT {
            detail = "草稿单据还没进过仓库，只改状态，不碰账".to_string();
        }
        // 可行性预检：库存够不够退 / 序列号有没有已出库 —— 拦路虎要现在就说
        if brief.status != stockin_model::STATUS_DRAFT {
            match stockin_model::ensure_cancellable(
                db(),
                brief.id,
                &brief.in_no,
                brief.warehouse_id,
            )
            .await
            {
                Ok(()) => {}
                Err(error) => {
                    let message = readable(&error.to_string());
                    blockers.push(format!("收货单 {} 不能作废：{message}", brief.in_no));
                    steps.push(CancelImpactItem {
                        exec_step: step,
                        doc_type: DOC_TYPE_STOCK_IN.to_string(),
                        doc_type_label: doc_type_label(DOC_TYPE_STOCK_IN).to_string(),
                        doc_id: brief.id.to_string(),
                        doc_no: brief.in_no.clone(),
                        doc_status: brief.status.clone(),
                        doc_status_label: stock_in_status_label(&brief.status),
                        amount: brief.total_with_tax,
                        action: ACTION_BLOCKED.to_string(),
                        action_label: "不能作废（先处理这个）".to_string(),
                        detail: message,
                    });
                    continue;
                }
            }
        }
        steps.push(CancelImpactItem {
            exec_step: step,
            doc_type: DOC_TYPE_STOCK_IN.to_string(),
            doc_type_label: doc_type_label(DOC_TYPE_STOCK_IN).to_string(),
            doc_id: brief.id.to_string(),
            doc_no: brief.in_no.clone(),
            doc_status: brief.status.clone(),
            doc_status_label: stock_in_status_label(&brief.status),
            amount: brief.total_with_tax,
            action: ACTION_EXECUTE.to_string(),
            action_label: "作废（回滚三本账 + 退序列号）".to_string(),
            detail,
        });

        // 凭证：随收货单作废在同一个事务里红冲
        if let Some(found) = voucher_of(brief.id).await? {
            steps.push(CancelImpactItem {
                exec_step: step,
                doc_type: "voucher".to_string(),
                doc_type_label: doc_type_label("voucher").to_string(),
                doc_id: found.id.to_string(),
                doc_no: found.voucher_no.clone(),
                doc_status: voucher::STATUS_POSTED.to_string(),
                doc_status_label: "已记账".to_string(),
                amount: found.amount,
                action: if found.reversed {
                    ACTION_SKIP.to_string()
                } else {
                    ACTION_EXECUTE.to_string()
                },
                action_label: if found.reversed {
                    "已红冲（跳过）".to_string()
                } else {
                    "自动红冲（生成红字凭证）".to_string()
                },
                detail: format!("随第 {step} 步在同一个事务里完成，原凭证保持已记账、打上红冲标记"),
            });
        }

        // 应付：随收货单作废自动作废（已付款的单不能作废 —— 拦路虎）
        if let Some(found) = payable_of_stock_in(brief.id).await? {
            let (action, label, detail) = if found.status == payable::STATUS_CANCELLED {
                (
                    ACTION_SKIP,
                    "已作废（跳过）".to_string(),
                    "这张应付单早前已经作废".to_string(),
                )
            } else if found.paid_amount > Decimal::ZERO {
                (
                    ACTION_BLOCKED,
                    "不能作废（已付过款）".to_string(),
                    format!(
                        "这张应付单已付 {}，作废单据表达不了「钱已付出去」——\
                         请先走付款冲销 / 红冲凭证",
                        found.paid_amount
                    ),
                )
            } else {
                (
                    ACTION_EXECUTE,
                    "自动作废".to_string(),
                    format!("随第 {step} 步在同一个事务里完成，在跑的付款审批一起终止"),
                )
            };
            if action == ACTION_BLOCKED {
                blockers.push(format!(
                    "应付单 {}（{}）已付款 {}，不能随收货单作废",
                    found.payable_no,
                    payable_status_label(&found.status),
                    found.paid_amount
                ));
            }
            steps.push(CancelImpactItem {
                exec_step: step,
                doc_type: DOC_TYPE_PAYABLE.to_string(),
                doc_type_label: doc_type_label(DOC_TYPE_PAYABLE).to_string(),
                doc_id: found.id.to_string(),
                doc_no: found.payable_no.clone(),
                doc_status: found.status.clone(),
                doc_status_label: payable_status_label(&found.status),
                amount: found.amount,
                action: action.to_string(),
                action_label: label,
                detail,
            });
        }
    }

    // 采购单自己
    exec_step += 1;
    let (action, label, detail) = match row.status.as_str() {
        purchase_model::STATUS_CANCELLED => (
            ACTION_SKIP,
            "已作废（跳过）".to_string(),
            "采购单已经是作废状态".to_string(),
        ),
        purchase_model::STATUS_CLOSED => {
            blockers.push(format!(
                "采购单 {} 已关闭（CLOSED），关闭态不能作废；请走状态流转记录说明",
                row.order_no
            ));
            (
                ACTION_BLOCKED,
                "不能作废（已关闭）".to_string(),
                "已关闭的采购单只能保留，不能作废".to_string(),
            )
        }
        _ => (
            ACTION_EXECUTE,
            "作废采购单".to_string(),
            "状态 → 已作废；在跑的审批单会先被终止，不留孤儿待办".to_string(),
        ),
    };
    steps.push(CancelImpactItem {
        exec_step,
        doc_type: DOC_TYPE_PURCHASE_ORDER.to_string(),
        doc_type_label: doc_type_label(DOC_TYPE_PURCHASE_ORDER).to_string(),
        doc_id: row.id.to_string(),
        doc_no: row.order_no.clone(),
        doc_status: row.status.clone(),
        doc_status_label: purchase_status_label(&row.status),
        amount: row.total_with_tax,
        action: action.to_string(),
        action_label: label,
        detail,
    });

    let affected = steps
        .iter()
        .filter(|item| item.action == ACTION_EXECUTE)
        .count();
    let summary = if affected == 0 {
        format!("作废 {} 不需要做任何事（下游都已处理）", row.order_no)
    } else {
        format!(
            "作废 {} 将影响以下 {} 张单据（本次会执行 {} 个动作）",
            row.order_no,
            steps.len(),
            affected
        )
    };

    Ok(CancelImpactRes {
        doc_type: DOC_TYPE_PURCHASE_ORDER.to_string(),
        doc_id: row.id.to_string(),
        doc_no: row.order_no,
        doc_status: row.status.clone(),
        doc_status_label: purchase_status_label(&row.status),
        amount: row.total_with_tax,
        can_cancel: blockers.is_empty(),
        summary,
        steps,
        blockers,
        note: "一键处理是**一个事务**：从红冲到作废采购单，任一步失败整笔回滚，\
               不会出现「废了一半」的中间态。"
            .to_string(),
    })
}

/// 收货单的影响分析。
async fn analyze_stock_in(id: Uuid) -> std::result::Result<CancelImpactRes, ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;

    let mut steps: Vec<CancelImpactItem> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let exec_step = 1u32;

    if row.status == stockin_model::STATUS_CANCELLED {
        steps.push(CancelImpactItem {
            exec_step: 0,
            doc_type: DOC_TYPE_STOCK_IN.to_string(),
            doc_type_label: doc_type_label(DOC_TYPE_STOCK_IN).to_string(),
            doc_id: row.id.to_string(),
            doc_no: row.in_no.clone(),
            doc_status: row.status.clone(),
            doc_status_label: stock_in_status_label(&row.status),
            amount: row.total_with_tax,
            action: ACTION_SKIP.to_string(),
            action_label: "已作废（跳过）".to_string(),
            detail: "这张收货单已经作废，库存只退一次".to_string(),
        });
    } else {
        if row.status != stockin_model::STATUS_DRAFT {
            match stockin_model::ensure_cancellable(db(), row.id, &row.in_no, row.warehouse_id)
                .await
            {
                Ok(()) => {}
                Err(error) => blockers.push(format!(
                    "收货单 {} 不能作废：{}",
                    row.in_no,
                    readable(&error.to_string())
                )),
            }
        }
        steps.push(CancelImpactItem {
            exec_step,
            doc_type: DOC_TYPE_STOCK_IN.to_string(),
            doc_type_label: doc_type_label(DOC_TYPE_STOCK_IN).to_string(),
            doc_id: row.id.to_string(),
            doc_no: row.in_no.clone(),
            doc_status: row.status.clone(),
            doc_status_label: stock_in_status_label(&row.status),
            amount: row.total_with_tax,
            action: if blockers.is_empty() {
                ACTION_EXECUTE.to_string()
            } else {
                ACTION_BLOCKED.to_string()
            },
            action_label: "作废（回滚三本账 + 退序列号）".to_string(),
            detail: "成本账 / 库位账 / 库存流水反向冲减，一物一码序列号软删".to_string(),
        });
        if let Some(found) = voucher_of(row.id).await? {
            steps.push(CancelImpactItem {
                exec_step,
                doc_type: "voucher".to_string(),
                doc_type_label: doc_type_label("voucher").to_string(),
                doc_id: found.id.to_string(),
                doc_no: found.voucher_no.clone(),
                doc_status: voucher::STATUS_POSTED.to_string(),
                doc_status_label: "已记账".to_string(),
                amount: found.amount,
                action: if found.reversed {
                    ACTION_SKIP.to_string()
                } else {
                    ACTION_EXECUTE.to_string()
                },
                action_label: if found.reversed {
                    "已红冲（跳过）".to_string()
                } else {
                    "自动红冲（生成红字凭证）".to_string()
                },
                detail: "随步骤 1 在同一个事务里完成".to_string(),
            });
        }
        if let Some(found) = payable_of_stock_in(row.id).await? {
            let paid = found.paid_amount > Decimal::ZERO;
            if paid {
                blockers.push(format!(
                    "应付单 {} 已付款 {}，不能随收货单作废",
                    found.payable_no, found.paid_amount
                ));
            }
            steps.push(CancelImpactItem {
                exec_step,
                doc_type: DOC_TYPE_PAYABLE.to_string(),
                doc_type_label: doc_type_label(DOC_TYPE_PAYABLE).to_string(),
                doc_id: found.id.to_string(),
                doc_no: found.payable_no.clone(),
                doc_status: found.status.clone(),
                doc_status_label: payable_status_label(&found.status),
                amount: found.amount,
                action: if found.status == payable::STATUS_CANCELLED {
                    ACTION_SKIP.to_string()
                } else if paid {
                    ACTION_BLOCKED.to_string()
                } else {
                    ACTION_EXECUTE.to_string()
                },
                action_label: if found.status == payable::STATUS_CANCELLED {
                    "已作废（跳过）".to_string()
                } else if paid {
                    "不能作废（已付过款）".to_string()
                } else {
                    "自动作废".to_string()
                },
                detail: "随步骤 1 在同一个事务里完成".to_string(),
            });
        }
    }

    let affected = steps
        .iter()
        .filter(|item| item.action == ACTION_EXECUTE)
        .count();
    Ok(CancelImpactRes {
        doc_type: DOC_TYPE_STOCK_IN.to_string(),
        doc_id: row.id.to_string(),
        doc_no: row.in_no.clone(),
        doc_status: row.status.clone(),
        doc_status_label: stock_in_status_label(&row.status),
        amount: row.total_with_tax,
        can_cancel: blockers.is_empty() && affected > 0,
        summary: format!(
            "作废收货单 {} 将影响以下 {} 张单据（本次会执行 {} 个动作）",
            row.in_no,
            steps.len(),
            affected
        ),
        steps,
        blockers,
        note:
            "收货单作废会同时把库存三本账退回去、红冲进项凭证、作废联动应付单 —— 都在一个事务里。"
                .to_string(),
    })
}

/// 应付单的影响分析。
async fn analyze_payable(id: Uuid) -> std::result::Result<CancelImpactRes, ApiError> {
    let row = payable_payment::find_payable_for_cascade(id).await?;
    let mut steps: Vec<CancelImpactItem> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let exec_step = 1u32;

    let (action, label, detail) = if row.status == payable::STATUS_CANCELLED {
        (
            ACTION_SKIP,
            "已作废（跳过）".to_string(),
            "这张应付单已经作废".to_string(),
        )
    } else if row.paid_amount > Decimal::ZERO {
        blockers.push(format!(
            "应付单 {} 已付款 {}，作废单据表达不了「钱已付出去」——请走付款冲销 / 红冲凭证",
            row.payable_no, row.paid_amount
        ));
        (
            ACTION_BLOCKED,
            "不能作废（已付过款）".to_string(),
            format!("已付 {}，只剩红冲凭证一条路", row.paid_amount),
        )
    } else if row.status == payable::STATUS_PENDING_CANCEL {
        (
            ACTION_EXECUTE,
            "作废（终止在跑的作废审批）".to_string(),
            "单据在作废审批中：一键处理会终止审批单并直接作废".to_string(),
        )
    } else {
        (
            ACTION_EXECUTE,
            "作废应付单".to_string(),
            "状态 → 已作废；进项凭证一起红冲，在跑的付款审批一起终止".to_string(),
        )
    };
    steps.push(CancelImpactItem {
        exec_step,
        doc_type: DOC_TYPE_PAYABLE.to_string(),
        doc_type_label: doc_type_label(DOC_TYPE_PAYABLE).to_string(),
        doc_id: row.id.to_string(),
        doc_no: row.payable_no.clone(),
        doc_status: row.status.clone(),
        doc_status_label: payable_status_label(&row.status),
        amount: row.amount,
        action: action.to_string(),
        action_label: label,
        detail,
    });
    if let Some(voucher_id) = row.voucher_id
        && let Some(found) = voucher_by_id(voucher_id).await?
    {
        steps.push(CancelImpactItem {
            exec_step,
            doc_type: "voucher".to_string(),
            doc_type_label: doc_type_label("voucher").to_string(),
            doc_id: found.id.to_string(),
            doc_no: found.voucher_no.clone(),
            doc_status: voucher::STATUS_POSTED.to_string(),
            doc_status_label: "已记账".to_string(),
            amount: found.amount,
            action: if found.reversed {
                ACTION_SKIP.to_string()
            } else {
                ACTION_EXECUTE.to_string()
            },
            action_label: if found.reversed {
                "已红冲（跳过）".to_string()
            } else {
                "自动红冲（生成红字凭证）".to_string()
            },
            detail: "随步骤 1 在同一个事务里完成".to_string(),
        });
    }

    let affected = steps
        .iter()
        .filter(|item| item.action == ACTION_EXECUTE)
        .count();
    Ok(CancelImpactRes {
        doc_type: DOC_TYPE_PAYABLE.to_string(),
        doc_id: row.id.to_string(),
        doc_no: row.payable_no.clone(),
        doc_status: row.status.clone(),
        doc_status_label: payable_status_label(&row.status),
        amount: row.amount,
        can_cancel: blockers.is_empty() && affected > 0,
        summary: format!(
            "作废应付单 {} 将影响以下 {} 张单据（本次会执行 {} 个动作）",
            row.payable_no,
            steps.len(),
            affected
        ),
        steps,
        blockers,
        note: "已付款的应付单不能作废（钱已经出去了）：请走付款冲销 / 红冲凭证。".to_string(),
    })
}

// ==================== 联动作废 ====================

/// 联动作废（`POST /business/erp/{doc_type}/{id}/cancel-cascade`）。
///
/// 一个事务、固定顺序、任一步失败整笔回滚。执行前再跑一次 [`analyze`]：
/// 有拦路虎就 409（**绝不跳过拦路虎硬做**），并把它写成可执行的一句话。
pub async fn cascade(
    doc_type: &str,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<CancelCascadeRes, ApiError> {
    let normalized = normalize_doc_type(doc_type)?;
    if normalized == DOC_TYPE_TRANSFER {
        return Err(bad_request(format!(
            "doc_type=transfer 不支持联动作废：{}",
            transfer_hint()
        )));
    }
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：作废会动多张单据的账，必须写明为什么（≥4 字）",
        ));
    };
    let len = reason.chars().count();
    if len < MIN_REASON_LEN {
        return Err(bad_request(format!(
            "reason 太短（{len} 字）：请写清作废原因（≥{MIN_REASON_LEN} 字）"
        )));
    }
    if len > MAX_REASON_LEN {
        return Err(bad_request(format!(
            "reason 超过 {MAX_REASON_LEN} 字（当前 {len} 字）"
        )));
    }
    // 统一加前缀：事后在 biz_status_log 里能一眼认出「这三张单是同一次联动作废废掉的」
    let log_reason = format!("联动作废：{reason}");

    // 执行前的影响分析：拦路虎当场拦下（不静默跳过）
    let impact = analyze(normalized, id).await?;
    if !impact.blockers.is_empty() {
        return Err(conflict(format!(
            "不能一键处理：{}",
            impact.blockers.join("；")
        )));
    }
    // 幂等：下游与单据本身都已经是终态 → 什么都不做，如实回报（别报错，用户点的可能就是「再确认一次」）
    let affected = impact
        .steps
        .iter()
        .filter(|item| item.action == ACTION_EXECUTE)
        .count();
    if affected == 0 {
        return Ok(CancelCascadeRes {
            doc_type: normalized.to_string(),
            doc_no: impact.doc_no.clone(),
            steps: impact
                .steps
                .iter()
                .map(|item| CancelCascadeStep {
                    seq: item.exec_step,
                    doc_type: item.doc_type.clone(),
                    doc_no: item.doc_no.clone(),
                    outcome: OUTCOME_SKIPPED.to_string(),
                    message: item.action_label.clone(),
                })
                .collect(),
            message: format!(
                "{} 及其下游都已处理过，本次没有需要执行的动作",
                impact.doc_no
            ),
            transaction: "未开事务：没有任何需要改动的行".to_string(),
        });
    }

    let now = Utc::now();
    let op = state_machine::Operator::of(operator);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启联动作废事务"))?;
    let outcome = match normalized {
        DOC_TYPE_PURCHASE_ORDER => {
            cascade_purchase_order_tx(&tx, id, &impact, &log_reason, &op, now).await
        }
        DOC_TYPE_STOCK_IN => cascade_stock_in_tx(&tx, id, &log_reason, &op, now).await,
        DOC_TYPE_PAYABLE => cascade_payable_tx(&tx, id, &log_reason, operator, now).await,
        other => Err(bad_request(format!("doc_type={other} 不支持联动作废"))),
    };

    match outcome {
        Ok(steps) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交联动作废事务"))?;
            let done = steps.iter().filter(|s| s.outcome == OUTCOME_DONE).count();
            tracing::info!(
                doc_type = normalized,
                doc_no = %impact.doc_no,
                operator = ?operator,
                steps = steps.len(),
                done,
                "联动作废完成（一个事务）"
            );
            Ok(CancelCascadeRes {
                doc_type: normalized.to_string(),
                doc_no: impact.doc_no.clone(),
                steps,
                message: format!(
                    "{} 已联动作废：{} 个动作在一个事务里完成",
                    impact.doc_no, done
                ),
                transaction: "单事务：任一步失败整笔回滚（不会留半笔账）".to_string(),
            })
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 给「第 N 步失败」套上可执行的上下文（用户要知道是哪一步没成）。
fn step_failure(seq: u32, label: &str, error: ApiError) -> ApiError {
    ApiError::new(
        error.status,
        error.code,
        format!(
            "第 {seq} 步「{label}」失败：{}；本次联动作废已整笔回滚（账一处没动）",
            error.msg
        ),
    )
}

/// 采购单的联动作废（事务体）。
pub(crate) async fn cascade_purchase_order_tx(
    tx: &Transaction,
    id: Uuid,
    impact: &CancelImpactRes,
    reason: &str,
    op: &state_machine::Operator,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<Vec<CancelCascadeStep>, ApiError> {
    let mut steps: Vec<CancelCascadeStep> = Vec::new();
    // ① 先锁采购单头（与收货过账 / 作废同一加锁顺序：采购单头 → 库存行）
    let header = tx
        .query_sql(
            "SELECT order_no, status FROM biz_purchase_order \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定采购订单"))?;
    let Some(row) = header.first() else {
        return Err(not_found("采购订单"));
    };
    let order_no: String = row
        .get_typed("order_no")
        .map_err(|error| utils::db_error(error, "读取采购单号"))?;

    // ② 逐张作废下游收货单（复用 P9/P16 的既有作废链路，一行回滚逻辑都不重写）
    let briefs = stock_ins_of_order(id).await?;
    let mut seq = 0u32;
    for brief in &briefs {
        if brief.status == stockin_model::STATUS_CANCELLED {
            continue;
        }
        seq += 1;
        let locked = stockin_query::lock_stock_in(tx, brief.id)
            .await
            .map_err(|error| utils::db_error(error, "锁定收货入库单"))?
            .ok_or_else(|| not_found("收货入库单"))?;
        stockin_handler_cancel::cancel_in_tx(tx, locked.id, &locked, Some(reason), op, now)
            .await
            .map_err(|error| step_failure(seq, &format!("作废收货单 {}", locked.in_no), error))?;
        steps.push(CancelCascadeStep {
            seq,
            doc_type: DOC_TYPE_STOCK_IN.to_string(),
            doc_no: locked.in_no.clone(),
            outcome: OUTCOME_DONE.to_string(),
            message: "已作废：库存三本账回滚 + 序列号退回 + 凭证红冲 + 应付作废".to_string(),
        });
    }

    // ③ 作废采购单本身（重算后的状态可能已被收货作废推回 APPROVED / PARTIAL_RECEIVED）
    let current: Option<String> = tx
        .query_sql(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "读取采购单状态"))?
        .first()
        .map(|row| row.get_typed::<String>("status"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取采购单状态"))?;
    let Some(current) = current else {
        return Err(not_found("采购订单"));
    };
    seq += 1;
    if current == purchase_model::STATUS_CANCELLED {
        steps.push(CancelCascadeStep {
            seq,
            doc_type: DOC_TYPE_PURCHASE_ORDER.to_string(),
            doc_no: order_no,
            outcome: OUTCOME_SKIPPED.to_string(),
            message: "已是作废状态，跳过".to_string(),
        });
        return Ok(steps);
    }
    // 在跑的审批单先终止（P11：不留孤儿待办，否则这张单以后再也发不起审批）
    if current == purchase_model::STATUS_SUBMITTED {
        purchase_bridge::cancel_approval_of(tx, id, &order_no, "采购单联动作废，审批自动终止")
            .await
            .map_err(|error| {
                step_failure(
                    seq,
                    "终止在跑的采购审批",
                    utils::db_error(error, "终止审批"),
                )
            })?;
    }
    state_machine::transition(
        tx,
        state_machine::ENTITY_PURCHASE_ORDER,
        id,
        &order_no,
        &purchase_model::LEGAL_EDGES,
        &current,
        purchase_model::STATUS_CANCELLED,
        op.clone(),
        Some(reason),
        None,
    )
    .await
    .map_err(|error| step_failure(seq, "作废采购单（状态机）", error))?;
    let affected = purchase_model::apply_status(
        tx,
        id,
        &current,
        purchase_model::STATUS_CANCELLED,
        Some(reason),
        op.id,
        now,
    )
    .await
    .map_err(|error| step_failure(seq, "作废采购单", utils::db_error(error, "作废采购单")))?;
    if affected == 0 {
        return Err(conflict(format!(
            "采购单 {order_no} 的状态在本次流转期间已被改动（期望 {current}），请刷新后重试"
        )));
    }
    steps.push(CancelCascadeStep {
        seq,
        doc_type: DOC_TYPE_PURCHASE_ORDER.to_string(),
        doc_no: order_no,
        outcome: OUTCOME_DONE.to_string(),
        message: format!("已作废（{current} → CANCELLED）"),
    });
    let _ = impact;
    Ok(steps)
}

/// 收货单的联动作废（事务体）。
pub(crate) async fn cascade_stock_in_tx(
    tx: &Transaction,
    id: Uuid,
    reason: &str,
    op: &state_machine::Operator,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<Vec<CancelCascadeStep>, ApiError> {
    let locked = stockin_query::lock_stock_in(tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if locked.status == stockin_model::STATUS_CANCELLED {
        return Ok(vec![CancelCascadeStep {
            seq: 0,
            doc_type: DOC_TYPE_STOCK_IN.to_string(),
            doc_no: locked.in_no.clone(),
            outcome: OUTCOME_SKIPPED.to_string(),
            message: "已作废，跳过".to_string(),
        }]);
    }
    stockin_handler_cancel::cancel_in_tx(tx, id, &locked, Some(reason), op, now)
        .await
        .map_err(|error| step_failure(1, &format!("作废收货单 {}", locked.in_no), error))?;
    Ok(vec![CancelCascadeStep {
        seq: 1,
        doc_type: DOC_TYPE_STOCK_IN.to_string(),
        doc_no: locked.in_no,
        outcome: OUTCOME_DONE.to_string(),
        message: "已作废：库存三本账回滚 + 序列号退回 + 凭证红冲 + 应付作废".to_string(),
    }])
}

/// 应付单的联动作废（事务体）。
pub(crate) async fn cascade_payable_tx(
    tx: &Transaction,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<Vec<CancelCascadeStep>, ApiError> {
    let row = payable_payment::lock_payable_in_tx(tx, id)
        .await?
        .ok_or_else(|| not_found("应付单"))?;
    if row.status == payable::STATUS_CANCELLED {
        return Ok(vec![CancelCascadeStep {
            seq: 0,
            doc_type: DOC_TYPE_PAYABLE.to_string(),
            doc_no: row.payable_no.clone(),
            outcome: OUTCOME_SKIPPED.to_string(),
            message: "已作废，跳过".to_string(),
        }]);
    }
    let payable_no = row.payable_no.clone();
    payable_payment::cancel_payable_in_tx(tx, id, reason, operator, now)
        .await
        .map_err(|error| step_failure(1, &format!("作废应付单 {payable_no}"), error))?;
    Ok(vec![CancelCascadeStep {
        seq: 1,
        doc_type: DOC_TYPE_PAYABLE.to_string(),
        doc_no: payable_no,
        outcome: OUTCOME_DONE.to_string(),
        message: "已作废：进项凭证红冲 + 付款审批终止".to_string(),
    }])
}

// ==================== HTTP 入口 ====================

/// 作废影响分析（**只读**，前端点「作废」时先调这个弹窗）。
///
/// # 出参
///
/// [`CancelImpactRes`]：受影响单据列表（类型 / 单号 / 金额 / 处理方式）+ 拦路虎。
///
/// # 异常情况
///
/// * 400 + `10001`：`doc_type` 不支持 / id 不是 UUID；
/// * 404 + `10002`：单据不存在或已删除。
#[nik::permission(
    path = "/business/erp/{doc_type}/{id}/cancel-impact",
    method = "GET",
    permission = "business:erp:doc:cancel-impact"
)]
#[nik::get("/business/erp/{doc_type}/{id}/cancel-impact")]
pub async fn cancel_impact_handler(
    Path((doc_type, id)): Path<(String, String)>,
) -> ResResult<CancelImpactRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(analyze(&doc_type, id).await?))
}

/// 联动作废（**一个事务**，顺序执行，失败整笔回滚）。
///
/// # 入参
///
/// [`CancelCascadeReq::reason`]：必填，≥4 字。
///
/// # 异常情况
///
/// * 400 + `10001`：`doc_type` 不支持 / reason 缺失或过短；
/// * 404 + `10002`：单据不存在或已删除；
/// * 409 + `10003`：有拦路虎（库存不足退 / 序列号已出库 / 应付单已付款 / 单据已关闭）。
///   报文里逐条列出拦路虎，前端原样展示。
#[nik::permission(
    path = "/business/erp/{doc_type}/{id}/cancel-cascade",
    method = "POST",
    permission = "business:erp:doc:cancel-cascade"
)]
#[nik::post("/business/erp/{doc_type}/{id}/cancel-cascade")]
pub async fn cancel_cascade_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((doc_type, id)): Path<(String, String)>,
    Json(req): Json<CancelCascadeReq>,
) -> ResResult<CancelCascadeRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(
        cascade(&doc_type, id, req.reason.as_deref(), operator_of(&claims)).await?,
    ))
}
