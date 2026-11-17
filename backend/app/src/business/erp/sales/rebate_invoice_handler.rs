//! 销售返利 · 红字发票接口层（M3-17）。前缀 `/api` 由 `[router].prefix` 注入，宏内写
//! `/business/erp/rebate-settlement/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET  | `/business/erp/rebate-settlement/{id}/invoice-preview` | `business:erp:rebate-settlement:invoice` | 试算票面（**不落库**：三列拆分 + 凭证分录 + 能不能开 + 拦在哪） |
//! | POST | `/business/erp/rebate-settlement/{id}/invoice` | `business:erp:rebate-settlement:invoice` | 生成红字发票（`OUT_RED`）+ 红字凭证，结算单转 `SETTLED` |
//!
//! # 本卡交付的是「二选一」的哪一支
//!
//! **红字发票**。取舍理由与金额口径写在 [`crate::business::erp::sales::rebate_invoice_dto`]
//! 的模块文档里（一句话：返利是销售折让，冲收入与销项税；本仓 `biz_invoice` 已有
//! `OUT_RED` 的一等公民支持与「程序造票」的成熟路径，而 `biz_payable` 是供应商台账）。
//!
//! # 三道业务闸（都拦在「写之前」，都报人能照着做的错）
//!
//! | 闸 | 拦什么 | 返回 |
//! |----|--------|------|
//! | ① 状态 | 只有 `DRAFT` 能开票；`SETTLED`（已开过）/ `CANCELLED`（已作废）都拒 | 409 + **66093** |
//! | ② 金额 | `rebate_amount` 必须 > 0（0 元票无业务含义，负返利属红冲/调账） | 409 + **66094** |
//! | ③ 期间 | 开票日所属会计期间已结账（`CLOSED`）→ 拒（否则凭证写进已关账期间） | 409 + **66095** |
//!
//! # 事务边界（一个事务装完五件事，中间不留死单）
//!
//! ```text
//! 锁结算单（FOR UPDATE）→ 三道闸 → 取发票号 → 插红字发票（DRAFT）
//!   → 状态机 DRAFT→ISSUED → 落红字凭证 → 发票回写 voucher_id
//!   → 结算单 DRAFT→SETTLED（WHERE status='DRAFT'，影响行数必须 1）
//! ```
//!
//! **提交后没有任何后续动作**：票、凭证、结算单状态在同一事务里同生共死，
//! 不存在「票开了但结算单还是 DRAFT」或「结算单已 SETTLED 但没票」的中间态
//! （库层 `ck_biz_srs_settled_link` 兜第二道）。
//!
//! # 为什么并发要锁结算单
//!
//! 两个请求同时给同一张 `DRAFT` 开票：都读到 `DRAFT` ⇒ 各开一张红字发票 ⇒
//! 收入与销项税被冲两遍（`rebate_amount` 只够冲一次）。`lock_and_load` 先
//! `SELECT … FOR UPDATE`，后到的那位读到的是 `SETTLED`，闸① 直接 409 + 66093。
//!
//! # 凭证科目（ARCH-7 §8.1 的口径，但按本仓实际科目表落地）
//!
//! ```text
//! 借 5001    主营业务收入       |不含税|
//! 借 2221.02 应交税费-销项税     |税额|      （税额为 0 时不出这一行）
//! 贷 2241.02 其他应付款-销售返利 |价税合计|  （V0440 种的末级科目；2241 本身是非末级）
//! ```

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::currency;
use crate::business::erp::invoice;
use crate::business::erp::sales::rebate_invoice_dto::{self as dto, InvoiceSplit, RebateInvoiceReq};
use crate::business::erp::sales::rebate_invoice_model::{self as model, NewRedInvoice, RedInvoiceRow};
use crate::business::erp::sales::rebate_settle_dto::{STATUS_CANCELLED, STATUS_SETTLED, status_label};
use crate::business::erp::sales::rebate_settle_model::{self as settle_model, SettlementRow};
use crate::business::erp::voucher::{self, EntryLine, PostVoucher};
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0440 的 sys_menu.auth_code 逐字一致） ====================

/// 试算票面 + 开红字发票（读写共用一个动作码：能看票面的人就是要开票的人）。
pub const PERM_INVOICE: &str = "business:erp:rebate-settlement:invoice";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const REBATE_INVOICE_PERMISSIONS: [&str; 1] = [PERM_INVOICE];

// ==================== 科目（ARCH-7 §8.1；按本仓实际科目表落地） ====================

/// 主营业务收入（V0183 种的是 `5001`，稿面的 `6001` 在本仓不存在）。
pub const ACCOUNT_REVENUE: &str = voucher::ACCOUNT_REVENUE;
/// 应交税费-销项税（叶子科目；`2221` 是汇总科目不许记账）。
pub const ACCOUNT_OUTPUT_TAX: &str = voucher::ACCOUNT_OUTPUT_TAX;
/// 其他应付款-销售返利（V0440 种的末级子科目；`2241` 在 V0226 已翻成非末级）。
pub const ACCOUNT_REBATE_PAYABLE: &str = "2241.02";

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`。
fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message.into())
}

/// `409 + 10003`（并发改状态这类「刷新再来」的冲突）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// `409 + 66095`：开票日所属会计期间已结账。
fn period_closed(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_INVOICE_PERIOD_CLOSED, message.into())
}

/// 写路径 DB 错误收口：约束名翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => conflict(format!(
            "{ctx}：唯一约束冲突{}（同一张红字发票只能被一张结算单认领；并发开票请刷新后重试）",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（票面符号 / 金额恒等 / 结算单状态联动）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        DbError::InvalidValue(text) if text.contains("已结账") => period_closed(format!("{ctx}：{text}")),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 参数解析 ====================

fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|error| bad_request(format!("{field} 不是合法 UUID：{error}")))
}

/// 开票日所属会计期间码（`yyyy-MM`；期间闸的键）。
fn period_code_of(day: nik::prelude::chrono::NaiveDate) -> String {
    day.format("%Y-%m").to_string()
}

// ==================== 编排：开红字发票 ====================

/// 生成红字发票（`DRAFT ──▶ SETTLED`）。
///
/// 参数与闸的说明见模块文档；这里只强调一件事：**所有写都在一个事务里**，
/// 任何一步失败都整体回滚，结算单仍是 `DRAFT`（可以修正后重来）。
pub async fn generate_of(
    operator: Option<Uuid>,
    id_raw: &str,
    req: RebateInvoiceReq,
) -> std::result::Result<dto::RebateInvoiceRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let tax_rate = dto::validate_tax_rate(req.tax_rate)?;
    let remark = dto::normalize_remark(req.remark.as_deref())?;
    let now = Utc::now();
    let invoice_date = req.invoice_date.unwrap_or_else(|| voucher::today_cst(now));

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启返利开红字发票事务"))?;

    // ① 锁 + 读结算单（并发开票在这里串行化）
    let settle = match model::lock_and_load(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(format!("返利结算单 {id} 不存在或已删除，无法开红字发票")));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定返利结算单"));
        }
    };

    // ② 闸①：状态（只有 DRAFT）
    if let Err(error) = dto::ensure_invoiceable(&settle.status) {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ③ 闸②：金额拆分（应返 > 0、三列同号、分位自洽）
    let split = match dto::split_invoice(settle.rebate_amount, tax_rate) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ④ 闸③：开票日所属期间不能是已结账
    let period_code = period_code_of(invoice_date);
    match voucher::period_status(&tx, &period_code).await {
        Ok(Some(status)) if status == voucher::PERIOD_CLOSED => {
            let _ = tx.rollback().await;
            return Err(period_closed(format!(
                "会计期间 {period_code} 已结账（开票日期 {invoice_date}）：已关账的月份不能再记账，\
                 红字发票的凭证写不进去。请把开票日期改到未结账的期间，或先反结账"
            )));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询会计期间状态"));
        }
    }

    // ⑤ 本位币（结算金额本身就是本位币口径 ⇒ 原币 = 本位币、汇率 1）
    let currency_code = match currency::base_currency_code(&tx).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读取账套本位币"));
        }
    };

    // ⑥ 取发票号（同一事务：推了号却没落库，那个号就永久占用）
    let invoice_no = match voucher::next_doc_no(&tx, &invoice::INVOICE_SEQ, now).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "取红字发票号"));
        }
    };

    let remark_text = remark.unwrap_or_else(|| auto_remark(&settle, tax_rate));

    // ⑦ 插红字发票（DRAFT）
    let new_invoice = NewRedInvoice {
        invoice_no: &invoice_no,
        customer_id: settle.customer_id,
        invoice_date,
        split,
        tax_rate,
        currency_code: &currency_code,
        settlement_no: &settle.settlement_no,
        remark: Some(remark_text.as_str()),
    };
    let invoice_id = match model::insert_red_invoice(&tx, &new_invoice, operator, now).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "生成红字发票"));
        }
    };

    // ⑧ 发票状态机留痕 + 推 ISSUED（与 `invoice::change_status` 同一条边表）
    if let Err(error) = state_machine::log_created(
        &tx,
        invoice::ENTITY,
        invoice_id,
        &invoice_no,
        invoice::STATUS_DRAFT,
        state_machine::Operator::of(operator),
        Some("销售返利红字票"),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(ApiError::new(error.status, error.code, error.msg));
    }
    if let Err(error) = state_machine::transition(
        &tx,
        invoice::ENTITY,
        invoice_id,
        &invoice_no,
        &invoice::LEGAL_EDGES,
        invoice::STATUS_DRAFT,
        invoice::STATUS_ISSUED,
        state_machine::Operator::of(operator),
        None,
        Some((state_machine::ENTITY_SALES_RETURN, settle.settlement_no.as_str())),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    match model::mark_invoice_issued(&tx, invoice_id, operator, now).await {
        Ok(1) => {}
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(conflict(format!(
                "红字发票 {invoice_no} 的状态在本次开票期间被改动，请重试"
            )));
        }
        Ok(_) => {
            let _ = tx.rollback().await;
            return Err(conflict("红字发票状态更新影响了多行，已回滚".to_string()));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "红字发票开票改状态"));
        }
    }

    // ⑨ 红字凭证（借收入 / 借销项税 / 贷其他应付款-销售返利）
    let voucher_id = match write_rebate_voucher(
        &tx,
        RebateVoucherArgs {
            settle: &settle,
            split,
            invoice_id,
            invoice_no: &invoice_no,
            invoice_date,
            operator,
            now,
        },
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) = model::attach_voucher(&tx, invoice_id, voucher_id, now).await {
        let _ = tx.rollback().await;
        return Err(write_error(error, "红字发票回写凭证"));
    }

    // ⑩ 结算单 DRAFT → SETTLED（影响行数必须 1）
    match model::mark_settled(&tx, settle.id, invoice_id, &invoice_no, voucher_id, operator, now).await {
        Ok(1) => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Err(conflict(format!(
                "结算单 {} 在开票期间已被改动（并发开票或刚被作废），本次开票已回滚，请刷新后重试",
                settle.settlement_no
            )));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "结算单转已开红字发票"));
        }
    }

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交返利开红字发票事务"))?;

    tracing::info!(
        settlement_no = %settle.settlement_no,
        invoice_no = %invoice_no,
        rebate_amount = %settle.rebate_amount,
        amount = %split.amount,
        tax_amount = %split.tax_amount,
        voucher_id = %voucher_id,
        "返利结算单已开红字发票（OUT_RED）"
    );

    // 回读（提交后走同一条读路径：单号 / 金额 / 凭证号都从库来，不用内存值拼）
    res_of(settle.id).await
}

/// 自动备注（人没写时给一句能看懂来源的话）。
fn auto_remark(settle: &SettlementRow, tax_rate: Decimal) -> String {
    format!(
        "销售返利 {} 红冲（{} 期间净销售含税返利 {}，税率 {}%）",
        settle.settlement_no, settle.period_code, settle.rebate_amount, tax_rate
    )
}

/// 红字凭证的入参（避免 `too_many_arguments`，与仓内 `PostVoucher` 同一手法）。
struct RebateVoucherArgs<'a> {
    /// 结算单（取单号 / 期间）。
    settle: &'a SettlementRow,
    /// 票面三列（全负）。
    split: InvoiceSplit,
    /// 红字发票 id。
    invoice_id: Uuid,
    /// 红字发票号。
    invoice_no: &'a str,
    /// 凭证日期（= 开票日期）。
    invoice_date: nik::prelude::chrono::NaiveDate,
    /// 操作人。
    operator: Option<Uuid>,
    /// 当前时刻。
    now: DateTime<Utc>,
}

/// 落红字凭证（`voucher::post_voucher` 是全部凭证写路径的唯一收口）。
async fn write_rebate_voucher(
    ex: &impl SqlExecutor,
    args: RebateVoucherArgs<'_>,
) -> std::result::Result<Uuid, ApiError> {
    let RebateVoucherArgs {
        settle,
        split,
        invoice_id,
        invoice_no,
        invoice_date,
        operator,
        now,
    } = args;
    let revenue_account = voucher::account_required(ex, ACCOUNT_REVENUE, "主营业务收入")
        .await
        .map_err(|error| utils::db_error(error, "取主营业务收入科目 5001"))?;
    let payable_account = voucher::account_required(ex, ACCOUNT_REBATE_PAYABLE, "其他应付款-销售返利")
        .await
        .map_err(|error| utils::db_error(error, "取其他应付款-销售返利科目 2241.02"))?;
    let summary = format!("销售返利 {} 红字发票 {}", settle.settlement_no, invoice_no);
    let mut lines = vec![
        // 借：主营业务收入（冲减）
        EntryLine {
            line_no: 1,
            summary: format!("{summary} 冲减收入"),
            account: revenue_account,
            debit: split.debit_revenue(),
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    if split.debit_tax() > Decimal::ZERO {
        let tax_account = voucher::account_required(ex, ACCOUNT_OUTPUT_TAX, "销项税额")
            .await
            .map_err(|error| utils::db_error(error, "取销项税额科目 2221.02"))?;
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 冲减销项税"),
            account: tax_account,
            debit: split.debit_tax(),
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    // 贷：其他应付款-销售返利（欠客户的返利负债）
    lines.push(EntryLine {
        line_no: 0,
        summary: format!("{summary} 应付客户返利"),
        account: payable_account,
        debit: Decimal::ZERO,
        credit: split.credit_payable(),
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    });
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_INVOICE,
            source_id: Some(invoice_id),
            source_no: Some(invoice_no),
            // 业务单号 = 结算单号：财务按 RB-… 查凭证要能查到
            source_doc_no: Some(settle.settlement_no.as_str()),
            voucher_date: invoice_date,
            summary: summary.clone(),
            lines,
            remark: Some("销售返利开红字发票：冲减收入与销项税，贷其他应付款-销售返利"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成返利红字凭证"))
}

// ==================== 编排：试算票面 ====================

/// 试算票面（**不落库**）：三列拆分 + 凭证分录 + 能不能开 + 拦在哪。
///
/// 不报 409：预览的作用就是「点开就能看见为什么不能开」，把 `can_generate = false`
/// 与 `blockers` 一起给前端，比让用户点两次按钮才知道好。
pub async fn preview_of(
    id_raw: &str,
    req: RebateInvoiceReq,
) -> std::result::Result<dto::RebateInvoicePreviewRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let tax_rate = dto::validate_tax_rate(req.tax_rate)?;
    let now = Utc::now();
    let invoice_date = req.invoice_date.unwrap_or_else(|| voucher::today_cst(now));

    let settle = settle_model::find_settlement(id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利结算单"))?
        .ok_or_else(|| not_found("返利结算单不存在或已删除"))?;

    let currency_code = currency::base_currency_code(db())
        .await
        .map_err(|error| utils::db_error(error, "读取账套本位币"))?;

    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if let Err(error) = dto::ensure_invoiceable(&settle.status) {
        blockers.push(error.msg);
    }

    let split = match dto::split_invoice(settle.rebate_amount, tax_rate) {
        Ok(value) => Some(value),
        Err(error) => {
            blockers.push(error.msg);
            None
        }
    };

    let period_code = period_code_of(invoice_date);
    match voucher::period_status(db(), &period_code).await {
        Ok(Some(status)) if status == voucher::PERIOD_CLOSED => blockers.push(format!(
            "会计期间 {period_code} 已结账（开票日期 {invoice_date}）：请把开票日期改到未结账期间，或先反结账"
        )),
        Ok(_) => {}
        Err(error) => return Err(utils::db_error(error, "查询会计期间状态")),
    }

    if settle.status == STATUS_SETTLED {
        warnings.push(format!(
            "该结算单已开过红字发票（票号 {}）：重复开票会重复冲减收入与销项税，本接口会拒绝",
            settle.invoice_no.clone().unwrap_or_else(|| "（未记录）".to_string())
        ));
    }
    if settle.status == STATUS_CANCELLED {
        warnings.push("该结算单已作废：作废单不产生返利，要返利请重新计算一张结算单".to_string());
    }
    if tax_rate != dto::default_tax_rate() {
        warnings.push(format!(
            "本次按 {tax_rate}% 拆分（默认 {}%）：与销售发票税率不一致时请确认口径",
            dto::default_tax_rate()
        ));
    }

    let voucher_lines = preview_voucher_lines(db(), split).await?;
    let existing_invoice = match settle.invoice_id {
        Some(invoice_id) => Some(invoice_res(&settle, invoice_id).await?),
        None => None,
    };

    Ok(dto::RebateInvoicePreviewRes {
        settlement_id: settle.id.to_string(),
        settlement_no: settle.settlement_no.clone(),
        settlement_status: settle.status.clone(),
        settlement_status_label: status_label(&settle.status).to_string(),
        customer_id: settle.customer_id.to_string(),
        customer_code: settle.customer_code.clone(),
        customer_name: settle.customer_name.clone(),
        settle_period: settle.settle_period.clone(),
        period_code: settle.period_code.clone(),
        rebate_amount: settle.rebate_amount,
        tax_rate,
        invoice_type: dto::INVOICE_TYPE_OUT_RED.to_string(),
        invoice_type_label: dto::INVOICE_TYPE_LABEL.to_string(),
        invoice_date,
        currency_code,
        invoice_amount: split.map_or(Decimal::ZERO, |value| value.amount),
        invoice_tax: split.map_or(Decimal::ZERO, |value| value.tax_amount),
        invoice_total: split.map_or(Decimal::ZERO, |value| value.total_with_tax),
        can_generate: blockers.is_empty() && split.is_some(),
        blockers,
        warnings,
        voucher_lines,
        existing_invoice,
    })
}

/// 分录预览（科目名称从库里取：预览里写死中文名，改名后就对不上了）。
async fn preview_voucher_lines(
    ex: &impl SqlExecutor,
    split: Option<InvoiceSplit>,
) -> std::result::Result<Vec<dto::VoucherLineRes>, ApiError> {
    let Some(split) = split else {
        return Ok(Vec::new());
    };
    let revenue = voucher::account_required(ex, ACCOUNT_REVENUE, "主营业务收入")
        .await
        .map_err(|error| utils::db_error(error, "取主营业务收入科目 5001"))?;
    let payable = voucher::account_required(ex, ACCOUNT_REBATE_PAYABLE, "其他应付款-销售返利")
        .await
        .map_err(|error| utils::db_error(error, "取其他应付款-销售返利科目 2241.02"))?;
    let mut lines = vec![dto::VoucherLineRes {
        side: "DEBIT".to_string(),
        account_code: revenue.code.clone(),
        account_name: revenue.name.clone(),
        amount: split.debit_revenue(),
        summary: "冲减主营业务收入".to_string(),
    }];
    if split.debit_tax() > Decimal::ZERO {
        let tax = voucher::account_required(ex, ACCOUNT_OUTPUT_TAX, "销项税额")
            .await
            .map_err(|error| utils::db_error(error, "取销项税额科目 2221.02"))?;
        lines.push(dto::VoucherLineRes {
            side: "DEBIT".to_string(),
            account_code: tax.code.clone(),
            account_name: tax.name.clone(),
            amount: split.debit_tax(),
            summary: "冲减销项税".to_string(),
        });
    }
    lines.push(dto::VoucherLineRes {
        side: "CREDIT".to_string(),
        account_code: payable.code.clone(),
        account_name: payable.name.clone(),
        amount: split.credit_payable(),
        summary: "应付客户返利".to_string(),
    });
    Ok(lines)
}

// ==================== 编排：回读 ====================

/// 按结算单 id 回读「已开的红字发票」（`POST` 的返回体）。
pub async fn res_of(settlement_id: Uuid) -> std::result::Result<dto::RebateInvoiceRes, ApiError> {
    let settle = settle_model::find_settlement(settlement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利结算单"))?
        .ok_or_else(|| not_found("返利结算单不存在或已删除"))?;
    let Some(invoice_id) = settle.invoice_id else {
        return Err(conflict(format!(
            "结算单 {} 还没有红字发票（状态 {}）：请先开票",
            settle.settlement_no,
            status_label(&settle.status)
        )));
    };
    invoice_res(&settle, invoice_id).await
}

/// 由结算单 + 发票 id 拼出回执。
async fn invoice_res(settle: &SettlementRow, invoice_id: Uuid) -> std::result::Result<dto::RebateInvoiceRes, ApiError> {
    let invoice = model::invoice_of(db(), invoice_id)
        .await
        .map_err(|error| utils::db_error(error, "查询红字发票"))?
        .ok_or_else(|| not_found(format!("红字发票 {invoice_id} 不存在或已删除")))?;
    let voucher_no = match invoice.voucher_id {
        Some(voucher_id) => model::voucher_no_of(db(), voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询红字凭证号"))?,
        None => None,
    };
    Ok(to_res(settle, &invoice, voucher_no))
}

/// 行 → 回执。
fn to_res(settle: &SettlementRow, invoice: &RedInvoiceRow, voucher_no: Option<String>) -> dto::RebateInvoiceRes {
    dto::RebateInvoiceRes {
        settlement_id: settle.id.to_string(),
        settlement_no: settle.settlement_no.clone(),
        settlement_status: settle.status.clone(),
        settlement_status_label: status_label(&settle.status).to_string(),
        customer_id: settle.customer_id.to_string(),
        customer_code: settle.customer_code.clone(),
        customer_name: settle.customer_name.clone(),
        settle_period: settle.settle_period.clone(),
        period_code: settle.period_code.clone(),
        rebate_amount: settle.rebate_amount,
        invoice_id: invoice.id.to_string(),
        invoice_no: invoice.invoice_no.clone(),
        invoice_type: invoice.invoice_type.clone(),
        invoice_type_label: dto::INVOICE_TYPE_LABEL.to_string(),
        invoice_date: invoice.invoice_date,
        invoice_amount: invoice.amount,
        invoice_tax: invoice.tax_amount,
        invoice_total: invoice.total_with_tax,
        tax_rate: invoice.tax_rate,
        currency_code: invoice.currency_code.clone(),
        invoice_status: invoice.status.clone(),
        voucher_id: invoice.voucher_id.map(|value| value.to_string()),
        voucher_no,
        settled_at: settle.settled_at,
    }
}

// ==================== HTTP 入口 ====================

/// 试算票面（不落库）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/{id}/invoice-preview",
    method = "POST",
    permission = "business:erp:rebate-settlement:invoice"
)]
#[nik::post("/business/erp/rebate-settlement/{id}/invoice-preview")]
pub async fn rebate_invoice_preview_api(
    Path(id): Path<String>,
    Json(req): Json<RebateInvoiceReq>,
) -> ResResult<dto::RebateInvoicePreviewRes> {
    Ok(Res::ok(preview_of(&id, req).await?))
}

/// 生成红字发票（`DRAFT → SETTLED`）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/{id}/invoice",
    method = "POST",
    permission = "business:erp:rebate-settlement:invoice"
)]
#[nik::post("/business/erp/rebate-settlement/{id}/invoice")]
pub async fn rebate_invoice_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RebateInvoiceReq>,
) -> ResResult<dto::RebateInvoiceRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(generate_of(operator, &id, req).await?))
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::sales::rebate_settle_dto::STATUS_DRAFT;

    /// 权限码与 V0440 逐字一致，且必须是写动作码（不是 :list）。
    #[test]
    fn permission_code_matches_migration() {
        assert_eq!(REBATE_INVOICE_PERMISSIONS, [PERM_INVOICE]);
        assert_eq!(PERM_INVOICE, "business:erp:rebate-settlement:invoice");
        assert!(!PERM_INVOICE.ends_with(":list"), "开票是写接口，不能复用列表码");
        println!("[M3-17] 权限码 rebate-settlement:invoice ✓");
    }

    /// 科目码与库里的种子一致（改错一个字，`account_required` 直接报「科目不存在」）。
    #[test]
    fn account_codes() {
        assert_eq!(ACCOUNT_REVENUE, "5001");
        assert_eq!(ACCOUNT_OUTPUT_TAX, "2221.02");
        assert_eq!(ACCOUNT_REBATE_PAYABLE, "2241.02");
        println!("[M3-17] 科目 5001 / 2221.02 / 2241.02 ✓");
    }

    /// 期间码只取年月（`yyyy-MM`），与 `biz_accounting_period.period_code` 同形。
    #[test]
    fn period_code_shape() {
        let day = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 17).expect("合法日期");
        assert_eq!(period_code_of(day), "2026-11");
        let edge = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期");
        assert_eq!(period_code_of(edge), "2026-01");
        println!("[M3-17] 期间码 yyyy-MM ✓");
    }

    /// 自动备注必须带结算单号与期间码（人靠这两个字段回溯「这笔红冲是哪来的」）。
    #[test]
    fn auto_remark_contains_traceability() {
        let settle = SettlementRow {
            id: Uuid::now_v7(),
            settlement_no: "RB-20261117-000001".to_string(),
            customer_id: Uuid::now_v7(),
            customer_code: Some("C001".to_string()),
            customer_name: Some("客户一".to_string()),
            settle_period: "MONTH".to_string(),
            period_code: "2026-11".to_string(),
            period_from: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            period_to: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
            policy_id: Uuid::now_v7(),
            policy_no: "BR-20261101-000001".to_string(),
            policy_name: "11 月返利".to_string(),
            rule_type: "BY_AMOUNT".to_string(),
            rate_unit: "PERCENT".to_string(),
            policy_effective_from: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            policy_effective_to: None,
            policy_snapshot: crate::business::erp::sales::rebate_settle_dto::PolicySnapshot {
                policy_no: "BR-20261101-000001".to_string(),
                policy_name: "11 月返利".to_string(),
                rule_type: "BY_AMOUNT".to_string(),
                rate_unit: "PERCENT".to_string(),
                effective_from: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                effective_to: None,
                tiers: Vec::new(),
            },
            base: Default::default(),
            tier_seq: Some(1),
            tier_from: Decimal::ZERO,
            tier_to: None,
            rate: Decimal::from(2),
            rebate_amount: Decimal::from(1000),
            status: STATUS_DRAFT.to_string(),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            invoice_id: None,
            invoice_no: None,
            voucher_id: None,
            settled_at: None,
            settled_by: None,
        };
        let text = auto_remark(&settle, Decimal::from(13));
        assert!(text.contains("RB-20261117-000001"), "备注必须带结算单号：{text}");
        assert!(text.contains("2026-11"), "备注必须带期间码：{text}");
        assert!(text.contains("13%"), "备注必须带税率：{text}");
        println!("[M3-17] 自动备注含单号/期间/税率 ✓：{text}");
    }
}
