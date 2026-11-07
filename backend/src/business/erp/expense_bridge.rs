//! 费用报销的**审批回调 + 凭证编排**（`expense` 流程，ARCH-6 §4.4.4 / §4.5）。
//!
//! # 四个入口（与 `payable_bridge` / `asset_bridge` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_expense_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 入账：计提凭证 + `usage CONSUMED` + `SUBMITTED → APPROVED`（**同一事务**）；其余终态 → 回 `DRAFT` + `usage RELEASED` + 意见写 `remark` |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 报销人 / 发起人不能批自己这一票 → 403 + 65019 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把报销人从审批人列表里排除（落 `SKIPPED` 留痕） |
//! | [`cancel_approval_of`] | `expense_handler::cancel`（作废审批中的单） | 终止在跑的审批实例，避免孤儿实例顶住 `uk_biz_flow_instance_biz_active` |
//!
//! # 为什么「同意」必须在一个事务里做四件事
//!
//! ```text
//! ① 生成计提凭证（借 费用科目×N + 借 2221.01 / 贷 2241.01）
//! ② 回写报销单：status APPROVED + voucher_id + period_code（按审批通过日重算）
//! ③ budget usage：OCCUPIED → CONSUMED（带 voucher_id）
//! ④ 状态机流转 SUBMITTED → APPROVED（写 biz_status_log）
//! ```
//!
//! 少任何一步都会留下「审批过了但没入账」（费用漏记，巡检 L5 BLOCKER）或
//! 「有凭证但单据还在审批中」的账，两种状态都没法自动判断哪个是对的。
//!
//! # 失败口径（与其它 bridge 一致，但报警级别更高）
//!
//! 回调跑在审批事务**提交之后**：业务侧失败**不许**回滚审批单，只记 `warn` 返回 `None`。
//! 但报销与采购不同——采购单卡在 `SUBMITTED` 只是显示问题；报销单卡在 `SUBMITTED`
//! 是「审批过了但没入账」= **费用漏记**，所以这里用 `error` 级别日志（ARCH-6 §4.4.4）。
//! 孤儿对账任务（`erp_expense_reconcile`）属 P1-5 卡，本卡先靠日志 + 巡检 L5 兜底。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::result::Result;

use crate::business::erp::expense_compute;
use crate::business::erp::expense_dto::{self, STATUS_APPROVED, STATUS_PAID, STATUS_SUBMITTED};
use crate::business::erp::expense_linkage;
use crate::business::erp::expense_model::{self, ItemRow, ReportRow};
use crate::business::erp::{budget_guard, voucher, voucher_multi};
use crate::business::flow::model as flow_model;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 报销审批的业务类型（与 V0243 种子 / `expense_handler::submit` 同口径）。
pub const BIZ_TYPE: &str = "EXPENSE";
/// 报销审批的流程编码。
pub const FLOW_CODE: &str = "expense";
/// 计提凭证的来源类型（`biz_voucher.source_type`）。
pub const SOURCE_REPORT: &str = "EXPENSE_REPORT";
/// 付款凭证的来源类型。
pub const SOURCE_PAYMENT: &str = "EXPENSE_PAYMENT";
/// 员工垫付科目（贷方）。
pub const ACCOUNT_EMPLOYEE: &str = "2241.01";
/// 进项税科目（可抵扣部分）。
pub const ACCOUNT_INPUT_TAX: &str = "2221.01";
/// 缺省付款账户（银行存款）。
pub const ACCOUNT_BANK: &str = "1002";
/// 费用兜底科目（规则表里的默认借方，只在明细缺科目时作为报错提示用）。
pub const ACCOUNT_EXPENSE_FALLBACK: &str = "5602.09";

/// 已发布的报销审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpenseFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的报销审批流程」。`None` = 没配（调用方 409 并指名去发布流程）。
pub async fn published_expense_flow() -> DbResult<Option<ExpenseFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'EXPENSE' OR lower(d.flow_code) = 'expense') \
              ORDER BY (d.biz_type = 'EXPENSE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(ExpenseFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 审批结论 → 是否要入账。
pub fn should_execute(instance_status: &str) -> bool {
    instance_status.trim().eq_ignore_ascii_case("APPROVED")
}

/// 审批结论 → 是否要**打回草稿**（驳回 / 撤回 / 终止）。
///
/// ⚠️ 这里必须**白名单否定终态**，不能写成 `!should_execute(..)`：
/// `approve_task` 在「多节点流程批完一站、实例还在跑」时回调传来的是 `PENDING`，
/// 用「非 APPROVED 就打回」会把刚批完第一站的在途单直接打回草稿（FIN-3 真机首跑踩到）。
/// 中间态一律什么都不做。
pub fn should_revert(instance_status: &str) -> bool {
    matches!(
        instance_status.trim().to_ascii_uppercase().as_str(),
        "REJECTED" | "REJECTED_TERMINATED" | "WITHDRAWN" | "CANCELLED" | "TERMINATED"
    )
}

// ==================== ① 审批结论回调 ====================

/// 审批单落终态 → 入账 / 打回（`FlowCallback` 身份）。
pub async fn on_expense_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    if !should_execute(instance_status) && !should_revert(instance_status) {
        // 中间态（PENDING / 单节点批完但实例在跑）：不入账也不打回
        tracing::debug!(%instance_id, instance_status, "报销审批进行中：不触发入账 / 打回");
        return None;
    }
    match should_execute(instance_status) {
        true => match execute_approval(instance_id).await {
            Ok(Some(report_id)) => {
                tracing::info!(%instance_id, %report_id, "报销审批通过：已入账（计提凭证 + 预算执行）");
                Some(report_id)
            }
            Ok(None) => None,
            Err(error) => {
                // ERROR 而不是 WARN：审批过了没入账 = 费用漏记（ARCH-6 §4.4.4 的报警口径）
                tracing::error!(
                    %instance_id,
                    code = error.code,
                    msg = %error.msg,
                    "报销入账失败（审批单不回滚）：单据仍停在 SUBMITTED，属「审批过了但没入账」，\
                     必须人工补做或等 erp_expense_reconcile 兜底"
                );
                None
            }
        },
        false => match revert_to_draft(instance_id).await {
            Ok(report_id) => {
                tracing::info!(%instance_id, instance_status, ?report_id, "报销审批未通过：回草稿并释放预算占用");
                report_id
            }
            Err(error) => {
                tracing::warn!(%instance_id, code = error.code, msg = %error.msg, "报销打回失败（审批单不回滚）");
                None
            }
        },
    }
}

/// 报销审批回调事务的返回值：单据 id + 待提交后跑的联动上下文。
///
/// 用结构体而不是元组：两个字段都是 `Option`，元组在调用处 `value.0 / value.1`
/// 读起来完全看不出哪个是单据、哪个是联动（这份代码里已经有太多 `Option<Uuid>`）。
#[derive(Default)]
struct ApprovalOutcome {
    /// 报销单 id（`None` = 单据 / 审批实例不存在，调用方按幂等处理）。
    report_id: Option<Uuid>,
    /// 事务内联动的执行结果（`deferred` 待 commit 之后跑）。
    linkage: Option<expense_linkage::ExpenseStatusChange>,
}

/// 入账（审批通过）：凭证 + 单据回写 + usage CONSUMED + 状态流转，**一个事务**。
async fn execute_approval(instance_id: Uuid) -> Result<Option<Uuid>, ApiError> {
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询报销审批单"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(report_id) = instance.biz_id else {
        return Ok(None);
    };
    let now = Utc::now();
    let approval_date = voucher::today_cst(now);
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启报销入账事务"))?;
    let outcome = async {
        let Some(report) = expense_model::report_lock_in(&tx, report_id)
            .await
            .map_err(|error| internal(error, "锁定报销单"))?
        else {
            return Ok::<ApprovalOutcome, ApiError>(ApprovalOutcome::default());
        };
        if report.status != STATUS_SUBMITTED {
            // 已经入账过（重复回调）或已被人工处理：幂等返回，不重复记账
            tracing::info!(
                report_no = %report.report_no,
                status = %report.status,
                "报销单不在 SUBMITTED（重复回调 / 已处理）：跳过入账"
            );
            return Ok(ApprovalOutcome {
                report_id: Some(report.id),
                linkage: None,
            });
        }
        let items = expense_model::items_of(&tx, report.id)
            .await
            .map_err(|error| internal(error, "查询报销明细"))?;
        // 跨表恒等式（巡检 L1 的现场版）：明细合计必须等于单头，否则**不许入账**
        check_totals(&report, &items)?;
        // 入账期间 = 审批通过日所属期间（ARCH-6 §4.5.4），不是费用发生日
        let period_code = voucher::period_of(&tx, approval_date)
            .await
            .map_err(|error| internal(error, "解析审批通过日所属会计期间"))?;
        let voucher_id = post_report_voucher(&tx, &report, &items, approval_date, now)
            .await
            .map_err(|error| voucher_error(error, "生成报销入账凭证", &report.report_no))?;
        let affected = expense_model::report_mark_approved(
            &tx,
            report.id,
            voucher_id,
            &period_code,
            Some(instance.initiator_id),
            now,
        )
        .await
        .map_err(|error| internal(error, "回写报销单为已审批"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!(
                    "报销单 {} 的状态在入账期间被改动，请刷新后重试",
                    report.report_no
                ),
            ));
        }
        // 预算占用 → 已执行（带 voucher_id，巡检 L8 用它核对口径）
        budget_guard::consume(
            &tx,
            budget_guard::SOURCE_EXPENSE,
            report.id,
            Some(voucher_id),
        )
        .await
        .map_err(|error| internal(error, "预算占用转已执行"))?;
        state_machine::transition(
            &tx,
            state_machine::ENTITY_EXPENSE,
            report.id,
            &report.report_no,
            &state_machine::EXPENSE_EDGES,
            STATUS_SUBMITTED,
            STATUS_APPROVED,
            instance.initiator_id,
            Some("报销审批通过并入账"),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ⑤ 跨单据联动（LNK-05）：状态已是 APPROVED。计提凭证在第 ① 步由
        //    post_report_voucher 出过了，GENERATE_VOUCHER 必须被排除清单挡掉，
        //    否则一张报销单两张计提凭证。
        let linkage =
            expense_linkage::run_in_tx(&tx, &report, STATUS_APPROVED, Some(instance.initiator_id))
                .await?;
        Ok(ApprovalOutcome {
            report_id: Some(report.id),
            linkage: Some(linkage),
        })
    }
    .await;
    match outcome {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| internal(error, "提交报销入账事务"))?;
            // 事务已提交 → 才跑提交后的联动动作（审批通过通知）
            if let Some(linkage) = value.linkage {
                expense_linkage::run_deferred_after_commit(linkage).await;
            }
            Ok(value.report_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 打回（驳回 / 撤回 / 终止）：回 `DRAFT` + 释放预算占用，一个事务。
async fn revert_to_draft(instance_id: Uuid) -> Result<Option<Uuid>, ApiError> {
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询报销审批单"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(report_id) = instance.biz_id else {
        return Ok(None);
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启报销打回事务"))?;
    let outcome = async {
        let Some(report) = expense_model::report_lock_in(&tx, report_id)
            .await
            .map_err(|error| internal(error, "锁定报销单"))?
        else {
            return Ok::<ApprovalOutcome, ApiError>(ApprovalOutcome::default());
        };
        if report.status != STATUS_SUBMITTED {
            return Ok(ApprovalOutcome {
                report_id: Some(report.id),
                linkage: None,
            });
        }
        let comment = instance
            .result_comment
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| format!("审批意见：{text}"));
        let affected = expense_model::report_mark_draft(
            &tx,
            report.id,
            comment.as_deref(),
            Some(instance.initiator_id),
        )
        .await
        .map_err(|error| internal(error, "报销单回草稿"))?;
        if affected == 0 {
            return Ok(ApprovalOutcome {
                report_id: Some(report.id),
                linkage: None,
            });
        }
        let reason = match instance.status.as_str() {
            "WITHDRAWN" => "WITHDRAWN",
            "CANCELLED" => "CANCELLED",
            _ => "REJECTED",
        };
        budget_guard::release(&tx, budget_guard::SOURCE_EXPENSE, report.id, reason)
            .await
            .map_err(|error| internal(error, "释放预算占用"))?;
        state_machine::transition(
            &tx,
            state_machine::ENTITY_EXPENSE,
            report.id,
            &report.report_no,
            &state_machine::EXPENSE_EDGES,
            STATUS_SUBMITTED,
            expense_dto::STATUS_DRAFT,
            instance.initiator_id,
            comment.as_deref().or(Some("审批未通过，退回草稿")),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ④ 跨单据联动（LNK-05）：状态已是 DRAFT。驳回 / 撤回都落这里 ——
        //    不接这一条，`EXPENSE_REPORT/DRAFT` 上配的规则又能保存、又能启用、永远不触发
        //    （P1-FIX-5C 那个病）。打回**不动账**（凭证在审批通过时才出，此时还没出），
        //    所以这里只有通知 / 待办类动作真会执行。
        let linkage = expense_linkage::run_in_tx(
            &tx,
            &report,
            expense_dto::STATUS_DRAFT,
            Some(instance.initiator_id),
        )
        .await?;
        Ok(ApprovalOutcome {
            report_id: Some(report.id),
            linkage: Some(linkage),
        })
    }
    .await;
    match outcome {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| internal(error, "提交报销打回事务"))?;
            // 事务已提交 → 才跑提交后的联动动作（打回通知）
            if let Some(linkage) = value.linkage {
                expense_linkage::run_deferred_after_commit(linkage).await;
            }
            Ok(value.report_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== ② 凭证 ====================

/// 生成计提凭证：借 费用科目（按科目聚合，带部门辅助）+ 借 2221.01 / 贷 2241.01。
pub async fn post_report_voucher(
    ex: &impl SqlExecutor,
    report: &ReportRow,
    items: &[ItemRow],
    voucher_date: NaiveDate,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let mut debit_input: Vec<(Uuid, Decimal)> = Vec::with_capacity(items.len());
    let mut tax_total = Decimal::ZERO;
    for item in items {
        debit_input.push((item.account_id, item.amount));
        tax_total += item.tax_amount;
    }
    let groups = expense_compute::debit_lines(&debit_input);
    if groups.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "报销单 {} 的明细金额全为 0：无法生成入账凭证",
            report.report_no
        )));
    }
    let mut debits = Vec::with_capacity(groups.len() + 1);
    for group in groups {
        let account = voucher_multi::ensure_leaf(ex, group.account_id).await?;
        debits.push(
            voucher_multi::DebitGroup::new(account, group.amount, Some(report.dept_id))
                .with_summary(format!("{} {}", report.report_no, report.title)),
        );
    }
    let tax_total = expense_compute::money(tax_total);
    if tax_total > Decimal::ZERO {
        let tax_account = voucher_multi::leaf_required(ex, ACCOUNT_INPUT_TAX).await?;
        debits.push(
            voucher_multi::DebitGroup::new(tax_account, tax_total, None)
                .with_summary(format!("{} 进项税", report.report_no)),
        );
    }
    let credit = voucher_multi::leaf_required(ex, ACCOUNT_EMPLOYEE).await?;
    let credit_amount = expense_compute::money(report.total_with_tax);
    voucher_multi::post_multi_debit(
        ex,
        &voucher_multi::MultiDebit {
            source_type: SOURCE_REPORT,
            source_id: Some(report.id),
            source_no: &report.report_no,
            voucher_date,
            summary: format!(
                "报销 {} {}（{}）",
                report.report_no, report.title, report.reporter_name
            ),
            debits,
            credit_account: credit,
            credit_amount,
            credit_summary: Some(format!("应付 {} 报销款", report.reporter_name)),
            remark: Some("ARCH-6 §4.5.1 ①：借 费用科目 + 借 进项税 / 贷 其他应付款-员工垫付"),
        },
        Some(report.reporter_id),
        now,
    )
    .await
}

/// 生成付款凭证：借 2241.01 / 贷 付款账户（缺省 1002 银行存款）。
pub async fn post_payment_voucher(
    ex: &impl SqlExecutor,
    report: &ReportRow,
    payment_no: &str,
    amount: Decimal,
    pay_date: NaiveDate,
    pay_account_code: &str,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let debit = voucher_multi::leaf_required(ex, ACCOUNT_EMPLOYEE).await?;
    let credit = voucher_multi::leaf_required(ex, pay_account_code).await?;
    let amount = expense_compute::money(amount);
    voucher_multi::post_multi_debit(
        ex,
        &voucher_multi::MultiDebit {
            source_type: SOURCE_PAYMENT,
            source_id: Some(report.id),
            source_no: payment_no,
            voucher_date: pay_date,
            summary: format!("报销付款 {} {}", report.report_no, report.reporter_name),
            debits: vec![voucher_multi::DebitGroup::new(
                debit,
                amount,
                Some(report.dept_id),
            )],
            credit_account: credit,
            credit_amount: amount,
            credit_summary: Some(format!("支付 {} 报销款", report.reporter_name)),
            remark: Some("ARCH-6 §4.5.1 ②：借 其他应付款-员工垫付 / 贷 银行存款"),
        },
        Some(report.reporter_id),
        now,
    )
    .await
}

/// 红冲某张报销单的全部蓝字凭证（作废已入账的单据）。
pub async fn reverse_report_vouchers(
    ex: &impl SqlExecutor,
    report_id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
) -> Result<Vec<Uuid>, ApiError> {
    let ids = expense_model::blue_voucher_ids(ex, SOURCE_REPORT, report_id)
        .await
        .map_err(|error| internal(error, "查询报销计提凭证"))?;
    let now = Utc::now();
    let mut reds = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(row) = voucher::find_voucher(ex, id)
            .await
            .map_err(|error| internal(error, "查询报销计提凭证"))?
        else {
            continue;
        };
        if row.reversed_by.is_some() {
            continue;
        }
        let red = voucher::reverse_posted(
            ex,
            &row,
            voucher::ReversalInput {
                voucher_date: voucher::today_cst(now),
                reason: Some(reason),
            },
            operator,
            now,
        )
        .await
        .map_err(|error| voucher_error(error, "红冲报销计提凭证", &row.voucher_no))?;
        reds.push(red);
    }
    Ok(reds)
}

// ==================== ③ 内控 ====================

/// 内控用：这张报销单的报销人（比 `create_by` 更准：财务代填时报销人 ≠ 制单人）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(report_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 按主键取 1 行的 reporter_id。
    let rows = db()
        .find_with_params(
            "SELECT reporter_id FROM biz_expense_report WHERE id = ?::uuid",
            vec![Value::from(&report_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("reporter_id"),
    }
}

/// 审批入口的内控闸（**事务之前**）：报销人 / 发起人不能批自己那一票 → 403 + 65019。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| internal(error, "查询待办"))?
    else {
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| internal(error, "查询审批单"))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    if instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：报销单的发起人不能审批自己提交的报销（请换一位审批人，或由他人代提）",
        ));
    }
    if let Some(report_id) = instance.biz_id
        && let Ok(Some(reporter)) = internal_control_creator(Some(report_id)).await
        && reporter == operator
    {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：报销人不能审批自己的报销单（请换一位审批人）",
        ));
    }
    Ok(())
}

/// 作废 / 删除审批中的报销单：终止在跑的实例（避免孤儿实例顶住唯一索引）。
///
/// 返回终止后的实例状态（`None` = 本来就没有实例）。
pub async fn cancel_approval_of(
    tx: &Transaction,
    report: &ReportRow,
) -> Result<Option<String>, ApiError> {
    let Some(instance_id) = report.approval_instance_id else {
        return Ok(None);
    };
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询报销审批单"))?
    else {
        return Ok(None);
    };
    if !instance.is_pending() {
        return Ok(Some(instance.status));
    }
    let out = crate::business::flow::engine::terminate_instance(
        tx,
        instance_id,
        "CANCELLED",
        "报销单作废：审批终止",
    )
    .await
    .map_err(|error| internal(error, "终止报销审批实例"))?;
    if let Some(rejected) = out.rejected {
        return Err(ApiError::new(rejected.status, rejected.code, rejected.msg));
    }
    Ok(Some("CANCELLED".to_string()))
}

// ==================== ④ 工具 ====================

/// 跨表恒等式复核（巡检 L1 的现场版）：单头金额 / 条数必须等于明细聚合。
pub fn check_totals(report: &ReportRow, items: &[ItemRow]) -> Result<(), ApiError> {
    if items.is_empty() {
        return Err(ApiError::new(
            422,
            error_code::ERP_EXPENSE_TOTAL_MISMATCH,
            format!(
                "报销单 {} 没有任何明细：无明细的单据不能入账",
                report.report_no
            ),
        ));
    }
    let count = i32::try_from(items.len()).unwrap_or(i32::MAX);
    let mut total = Decimal::ZERO;
    let mut tax = Decimal::ZERO;
    for item in items {
        total += item.with_tax_amount;
        tax += item.tax_amount;
    }
    let total = expense_compute::money(total);
    let tax = expense_compute::money(tax);
    if count != report.item_count || total != report.total_with_tax || tax != report.total_tax {
        return Err(ApiError::new(
            422,
            error_code::ERP_EXPENSE_TOTAL_MISMATCH,
            format!(
                "报销单 {} 的明细与单头对不上（明细 {} 条 / 价税 {} / 税额 {} ≠ \
                 单头 {} 条 / 价税 {} / 税额 {}）：这是 ERP 大忌，入账前必须先修数据",
                report.report_no,
                count,
                total,
                tax,
                report.item_count,
                report.total_with_tax,
                report.total_tax
            ),
        ));
    }
    Ok(())
}

/// `DbError` → `ApiError`：凭证侧的「期间已结账 / 已红冲」是业务冲突（409），
/// 不能一律翻成 500（否则前端只看到「内部错误」）。
pub fn voucher_error(error: DbError, ctx: &str, doc_no: &str) -> ApiError {
    match error {
        DbError::InvalidValue(msg) => {
            let closed = msg.contains("已结账") || msg.contains("未开账");
            let status = if closed || msg.contains("已经红冲") {
                409
            } else {
                400
            };
            let code = if closed {
                error_code::ERP_EXPENSE_PERIOD_CLOSED
            } else {
                error_code::PARAM_INVALID
            };
            ApiError::new(status, code, format!("{ctx}（{doc_no}）：{msg}"))
        }
        other => internal(other, ctx),
    }
}

/// 内部错误。
pub fn internal(error: DbError, ctx: &str) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("{ctx}失败：{error}"),
    )
}

/// 付清 / 部分付款后的目标状态（付款事务里用它算 `status`）。
pub fn paid_status(pay_status: &str, _current: &str) -> &'static str {
    // 只有「付清」才翻 PAID；部分付款不改单据状态（`pay_status` 自己记 PARTIAL）。
    // 已 PAID 的单不会再走到这里（付款入口对 `pay_status = PAID` 直接 409）。
    if pay_status == expense_dto::PAY_PAID {
        STATUS_PAID
    } else {
        STATUS_APPROVED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 只有否定终态才打回草稿；PENDING 等中间态必须什么都不做。
    ///
    /// 这是 FIN-3 真机首跑踩到的真实缺陷：`approve_task` 批完第一站时回调传的是
    /// `PENDING`，用「非 APPROVED 就打回」会把在途单直接打回草稿。
    #[test]
    fn only_negative_terminal_reverts() {
        assert!(should_revert("REJECTED"));
        assert!(should_revert("WITHDRAWN"));
        assert!(should_revert("CANCELLED"));
        assert!(should_revert("rejected_terminated"));
        assert!(!should_revert("PENDING"));
        assert!(!should_revert("APPROVED"));
        assert!(!should_revert(""));
    }

    /// ② 只有 APPROVED 才入账（驳回 / 撤回 / 终止都不动账）。
    #[test]
    fn only_approved_books() {
        assert!(should_execute("APPROVED"));
        assert!(should_execute("approved"));
        assert!(!should_execute("REJECTED"));
        assert!(!should_execute("WITHDRAWN"));
        assert!(!should_execute("CANCELLED"));
        assert!(!should_execute("PENDING"));
    }

    /// ② 报销边表与状态机注册表逐条一致（防两处漂移）。
    #[test]
    fn edges_match_registry() {
        assert_eq!(
            state_machine::EXPENSE_EDGES.as_slice(),
            state_machine::Entity::ExpenseReport.edges(),
            "expense_bridge 的边表必须与状态机注册表完全一致"
        );
        assert_eq!(
            state_machine::EXPENSE_REASON_EDGES.as_slice(),
            state_machine::Entity::ExpenseReport.reason_edges(),
            "需要原因的三条边也必须同源"
        );
    }

    /// ③ 付清 → PAID；只付了一部分 → 仍 APPROVED（pay_status 才是 PARTIAL）。
    #[test]
    fn paid_status_mapping() {
        assert_eq!(paid_status("PAID", "APPROVED"), STATUS_PAID);
        assert_eq!(paid_status("PARTIAL", "APPROVED"), STATUS_APPROVED);
        assert_eq!(paid_status("UNPAID", "APPROVED"), STATUS_APPROVED);
        assert_eq!(paid_status("PAID", "PAID"), STATUS_PAID);
    }

    /// ④ 凭证错误：期间已结账 → 409 + 66043（不能翻成 500）。
    #[test]
    fn period_closed_maps_to_409() {
        let error = DbError::InvalidValue("会计期间 2026-11 已结账，不能记账".to_string());
        let api = voucher_error(error, "生成报销入账凭证", "EXP-1");
        assert_eq!(api.status, 409);
        assert_eq!(api.code, error_code::ERP_EXPENSE_PERIOD_CLOSED);
        let other = DbError::InvalidValue("科目 5602 不是末级科目".to_string());
        let api = voucher_error(other, "生成报销入账凭证", "EXP-1");
        assert_eq!(api.status, 400);
    }
}
