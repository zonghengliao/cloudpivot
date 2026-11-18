//! 框架协议审批的**业务回调 + 内控 + 流程查询**（M4-11b）。
//!
//! 与 `purchase_plan_bridge`（采购计划审批）、`supplier_access_flow`（供应商准入审批）
//! 并列的业务回调：审批单落终态之后，把结论翻译成框架协议单的状态。**彼此互不影响**。
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_fwa_settled`] | `erp::doc_reader_impl` 的 `ErpDoc::settle`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//! | [`published_fwa_flow`] | `framework_agreement_handler::submit_agreement` | 查「已发布的 `fw_agreement` 流程」；`None` = 没配（默认拒绝 + 显式跳过） |
//!
//! # 状态域只有三个值（与表结构口径一致）
//!
//! ```text
//! DRAFT（拟稿，可改可删）→ SUBMITTED（审批中，必有实例）→ APPROVED（生效，可被下单引用）
//! 驳回 / 撤回由本回调打回 DRAFT，而不是造一个「已驳回」状态 ——
//! 打回后的人要做的事和草稿完全一样（改完再提），多一个状态只会让按钮判定分叉。
//! ```
//!
//! **提交与通过都不碰主数据、不碰执行量**：协议本身没有账，`executed_qty` 是采购单回写的
//! 快照（M4-08），审批结论不影响它 —— 所以「驳回」「撤回」没有任何需要回滚的副作用。
//!
//! # 时序与失败口径（与 `purchase_plan_bridge` / `supplier_access_flow` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。协议单没有账，最坏情况是「审批过了、单据显示审批中」，
//! 下一轮人工能推进，不产生错账。
//!
//! # 「事务提交后失败必须有补偿」在本卡的应用（AGENTS 约定 11）
//!
//! 提交审批的免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后
//! **立即回调** [`on_fwa_settled`]；它失败时单据停在 `SUBMITTED` 且没有待办 ——
//! 这正是约定 11 点名的最高危形态。所以提交侧在回调失败时**显式打回草稿**
//! （见 `framework_agreement_handler::submit_agreement` 的 `auto_finished` 分支）：
//! 用户能重新提交，不需要工程师改库。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::framework_agreement_model::{
    self as model, AgreementItemRow, AgreementRow, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::business::shared::separation;
use cloudpivot_kernel::shared::error_code;

/// 框架协议审批的业务类型（`biz_flow_instance.biz_type` 落库值，与 V0456 种子同口径）。
pub const BIZ_TYPE: &str = "FWA_APPROVE";
/// 框架协议审批的流程编码（`biz_flow_def.flow_code`，与 V0456 种子同口径）。
pub const FLOW_CODE: &str = "fw_agreement";

/// 已发布的框架协议审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwaFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`FWA_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的框架协议审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按「默认拒绝 + 显式跳过」处理
/// （`approval_gate::authorize_missing_flow`：持 `skip-approval` 码 + 理由 ≥10 字才放行）。
///
/// 排序口径与 `purchase_plan_bridge::published_pp_flow` 完全一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_fwa_flow() -> DbResult<Option<FwaFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'FWA_APPROVE' OR lower(d.flow_code) = 'fw_agreement') \
              ORDER BY (d.biz_type = 'FWA_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(FwaFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 协议单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FwaVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（协议生效，可被下单引用）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl FwaVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            FwaVerdict::Approve => "APPROVE",
            FwaVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动协议单）。
pub fn fwa_verdict(instance_status: &str) -> Option<FwaVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(FwaVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(FwaVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进框架协议状态（`DocReader` 回调身份）。
pub async fn on_fwa_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = fwa_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(agreement_id)) => {
            tracing::info!(
                %instance_id,
                %agreement_id,
                verdict = verdict.as_str(),
                "框架协议审批结论已回调单据"
            );
            Some(agreement_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "框架协议审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_fwa_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: FwaVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「框架协议审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过框架协议审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(agreement_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在
    let Some(row) = model::find_agreement(db(), agreement_id).await? else {
        tracing::warn!(%agreement_id, "框架协议不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 状态闸：只有 SUBMITTED 能接受审批结论（已被别的路径推走 → 不动）
    if row.status != STATUS_SUBMITTED {
        tracing::debug!(
            %agreement_id,
            status = %row.status,
            verdict = verdict.as_str(),
            "框架协议当前状态不接受审批结论（跳过）"
        );
        return Ok(None);
    }
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        FwaVerdict::Approve => (STATUS_APPROVED, "框架协议审批通过".to_string()),
        FwaVerdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };
    let now = Utc::now();
    let tx = db().begin().await?;
    let outcome = async {
        let affected = if to_status == STATUS_APPROVED {
            model::mark_approved(&tx, agreement_id, Some(&reason), approver, now).await?
        } else {
            model::mark_back_to_draft(&tx, agreement_id, Some(&reason), approver, now).await?
        };
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "框架协议状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(agreement_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                %agreement_id,
                code = error.code,
                msg = %error.msg,
                "框架协议审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 内控：制单人不能批自己的协议 ====================

/// 内控用：这份协议的**制单人**（`biz_framework_agreement.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(agreement_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_framework_agreement WHERE id = ?::uuid",
            vec![Value::from(&agreement_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("create_by")?),
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// 判据（任一命中即 403）：操作人 == 协议单的 `create_by`（制单人），
/// 或操作人 == 审批单的 `initiator_id`（发起人）。
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("查询待办失败：{error}")))?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("查询审批单失败：{error}")))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    // ERP-B2：「制单与审批分离」开关（默认开 = 原有内控闸）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(agreement_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_agreement(db(), agreement_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的框架协议：协议 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.agreement_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

/// 最近一条**已完成**待办的审批人（多节点取最后一票）。读不到 → `None`。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok().flatten()),
    }
}

/// 最近一条**驳回**意见（写进协议 `remark`，让人看到被打回的理由）。
async fn latest_reject_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok().flatten()),
    }
}

// ==================== 审批表单变量 ====================

/// 审批表单变量（条件边的作用域；协议流程可据此做「金额分档」「浮动价加签」）。
///
/// ⚠️ 金额 / 数量必须写成 JSON **数字**（不是字符串）：条件边比较走 `as_f64()`，
/// `"500000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边
/// （也就是大额协议悄悄跳过财务复核，这是最危险的一类静默失败）。
pub fn form_data(row: &AgreementRow, items: &[AgreementItemRow]) -> serde_json::Value {
    let lines: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "line_no": item.line_no,
                "material_id": item.material_id.to_string(),
                "material_code": item.material_code,
                "material_name": item.material_name,
                "unit_name": item.unit_name,
                "agreed_price": item.agreed_price.to_string(),
                "tax_rate": item.tax_rate.to_string(),
                "max_qty": item.max_qty.map(|value| value.to_string()),
            })
        })
        .collect();
    serde_json::json!({
        "agreement_no": row.agreement_no,
        "agreement_name": row.agreement_name,
        "supplier_id": row.supplier_id.to_string(),
        "supplier_name": row.supplier_display_name,
        "start_date": row.start_date.to_string(),
        "end_date": row.end_date.to_string(),
        "price_mode": row.price_mode,
        "price_includes_tax": row.price_includes_tax,
        "price_currency": row.price_currency,
        "item_count": row.item_count,
        // 条件边读这两个键（**数字**，不是字符串）
        "total_max_qty": json_number(row.total_max_qty.map(|value| value.round_dp(4).to_string())),
        "total_max_amount": json_number(row.total_max_amount.map(|value| value.round_dp(4).to_string())),
        "items": lines,
    })
}

/// 十进制字符串 → JSON **数字**（`None` → `null`；条件边比较走 `as_f64()`）。
fn json_number(decimal_text: Option<String>) -> serde_json::Value {
    decimal_text
        .and_then(|text| text.parse::<f64>().ok())
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::Decimal;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn head() -> AgreementRow {
        AgreementRow {
            id: Uuid::nil(),
            agreement_no: "FWA-20261118-000001".to_string(),
            agreement_name: "2026 年度钢材框架采购协议".to_string(),
            supplier_id: Uuid::nil(),
            supplier_name: Some("快照名".to_string()),
            supplier_display_name: "现名".to_string(),
            dept_id: None,
            dept_name: None,
            owner_id: None,
            owner_name: None,
            start_date: date("2026-01-01"),
            end_date: date("2026-12-31"),
            sign_date: None,
            price_mode: model::PRICE_MODE_FLOATING.to_string(),
            price_includes_tax: false,
            price_currency: "CNY".to_string(),
            price_tolerance: dec("0.05"),
            price_adjust_rule: Some("随指数月度上浮不超过 3%".to_string()),
            total_max_qty: Some(dec("1000")),
            total_max_amount: Some(dec("500000.0000")),
            total_executed_qty: Decimal::ZERO,
            total_executed_amount: Decimal::ZERO,
            status: STATUS_DRAFT.to_string(),
            approval_instance_id: None,
            remark: None,
            created_at: Utc::now(),
            create_by: None,
            item_count: 1,
        }
    }

    /// ① 实例状态 → 业务语义（只有 APPROVED 才能变已批准）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(fwa_verdict("APPROVED"), Some(FwaVerdict::Approve));
        assert_eq!(fwa_verdict(" approved "), Some(FwaVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                fwa_verdict(status),
                Some(FwaVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(fwa_verdict(status), None, "{status} 不该推进协议单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0456 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "FWA_APPROVE");
        assert_eq!(FLOW_CODE, "fw_agreement");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        assert_eq!(FLOW_CODE, FLOW_CODE.to_lowercase());
    }

    /// ③ 表单快照：金额/数量必须是 JSON **数字**（字符串会让条件边静默走兜底）。
    #[test]
    fn form_data_uses_json_numbers() {
        let items = vec![AgreementItemRow {
            id: Uuid::nil(),
            agreement_id: Uuid::nil(),
            line_no: 1,
            material_id: Uuid::nil(),
            material_code: "M-001".to_string(),
            material_name: "钢卷".to_string(),
            unit_id: Uuid::nil(),
            unit_name: "吨".to_string(),
            agreed_price: dec("3800"),
            tax_rate: dec("0.13"),
            max_qty: Some(dec("100")),
            executed_qty: Decimal::ZERO,
            executed_amount: Decimal::ZERO,
            remark: None,
        }];
        let value = form_data(&head(), &items);
        assert!(value["total_max_qty"].is_number(), "数量上限必须是数字，不能是字符串");
        assert!(value["total_max_amount"].is_number(), "金额上限必须是数字");
        assert_eq!(value["total_max_amount"].as_f64(), Some(500000.0));
        assert_eq!(value["item_count"].as_i64(), Some(1));
        assert_eq!(value["supplier_name"], serde_json::json!("现名"));
        assert_eq!(value["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["items"][0]["material_name"], serde_json::json!("钢卷"));
        // 不限量（None）→ null，不能折成 0（0 会被条件边读成「上限 0」）
        let mut unlimited = head();
        unlimited.total_max_qty = None;
        assert!(form_data(&unlimited, &items)["total_max_qty"].is_null());
    }

    /// ④ 状态常量不能写错（错一个字就是库层 check 违约 / 前端徽标空白）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_DRAFT, "DRAFT");
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
    }
}
