//! 资产处置审批的**业务回调**（`asset_disposal`）：审批结论 → 处置凭证 + 卡片状态。
//!
//! # 三个入口（与 `budget_bridge` / `purchase_bridge` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_disposal_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 生成处置凭证（出售 3 张 / 报废 2 张 / 盘亏 2 张）+ 卡片置 `DISPOSED` + 写变动留痕；`REJECTED` / `WITHDRAWN` / `CANCELLED` → 资产**原地不动**（一张凭证都不生成） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 发起人 / 制单人不能批自己这一票 → 403 + 65019 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 为什么「同意」这一步必须在一个事务里做四件事
//!
//! ```text
//! ① 生成处置凭证（1~3 张，1606 / 1901 必须归零 —— 巡检 K7）
//! ② 卡片置 DISPOSED + 回填 disposal_type / disposal_date / disposal_voucher_id
//! ③ 状态机流转 IN_USE|IDLE → DISPOSED（写 biz_status_log）
//! ④ 写 biz_fixed_asset_change（DISPOSAL，append-only 留痕）
//! ```
//!
//! 少任何一步都会留下「凭证有了但卡片还在役」或「卡片已处置但查不到为什么」的账 ——
//! 这两种状态都没法自动修复（只能人工补单），所以必须同事务。
//!
//! # 失败口径（与其它 bridge 一致）
//!
//! 回调跑在审批事务**提交之后**：审批结论已落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None` —— 资产停在 `IN_USE` 且没有处置凭证，
//! 人工可以「重新发起处置」或按审批单补处置（这是**可重试**的失败，
//! 不会产生错账：没有凭证就没有账务后果）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::result::Result;

use crate::business::erp::asset_compute::{self, DisposalAccounts, DisposalAmounts, DisposalKind};
use crate::business::erp::asset_dto::{self, status_label};
use crate::business::erp::asset_linkage;
use crate::business::erp::asset_model::{self, AssetRow};
use crate::business::erp::voucher;
use crate::business::erp::voucher_multi;
use crate::business::flow::model as flow_model;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 资产处置审批的业务类型（与 V0231 种子 / `asset_handler::dispose` 同口径）。
pub const BIZ_TYPE: &str = "ASSET_DISPOSAL";
/// 资产处置审批的流程编码。
pub const FLOW_CODE: &str = "asset_disposal";
/// 处置凭证的来源类型（`biz_voucher.source_type`）。
pub const SOURCE_DISPOSAL: &str = "FIXED_ASSET_DISPOSAL";

/// 已发布的资产处置审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposalFlow {
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

/// 查「已发布的资产处置审批流程」。`None` = 没配（调用方 409 并指名去发布流程）。
pub async fn published_disposal_flow() -> DbResult<Option<DisposalFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'ASSET_DISPOSAL' OR lower(d.flow_code) = 'asset_disposal') \
              ORDER BY (d.biz_type = 'ASSET_DISPOSAL') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(DisposalFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 审批结论 → 是否要执行处置。
pub fn should_execute(instance_status: &str) -> bool {
    instance_status.trim().eq_ignore_ascii_case("APPROVED")
}

/// 审批表单 → 处置参数（**字段名与 `asset_handler::dispose` 写的一致，不许漂移**）。
pub fn disposal_spec_of(
    form: &serde_json::Value,
) -> Result<(DisposalKind, NaiveDate, String, Decimal, Decimal), ApiError> {
    let kind_raw = form
        .get("disposal_type")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let kind = DisposalKind::parse(kind_raw).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("处置审批单里的 disposal_type「{kind_raw}」不是合法取值"),
        )
    })?;
    let date_raw = form
        .get("disposal_date")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let date = NaiveDate::parse_from_str(date_raw, "%Y-%m-%d").map_err(|err| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("处置审批单里的 disposal_date「{date_raw}」无法解析：{err}"),
        )
    })?;
    let reason = form
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let sale_price = amount_of(form, "sale_price");
    let output_tax = amount_of(form, "output_tax");
    Ok((kind, date, reason, sale_price, output_tax))
}

/// `form_data` 里的金额（JSON 字符串或数字都认；缺省 0）。
fn amount_of(form: &serde_json::Value, key: &str) -> Decimal {
    match form.get(key) {
        Some(serde_json::Value::String(text)) => text.parse::<Decimal>().unwrap_or(Decimal::ZERO),
        Some(serde_json::Value::Number(number)) => number.to_string().parse::<Decimal>().unwrap_or(Decimal::ZERO),
        _ => Decimal::ZERO,
    }
}

/// 审批单落终态 → 执行处置（`FlowCallback` 身份）。
pub async fn on_disposal_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    if !should_execute(instance_status) {
        // 驳回 / 撤回 / 终止：资产原地不动，只留一条 info 便于对账
        tracing::info!(%instance_id, instance_status, "资产处置审批未通过：资产保持原状态，未生成任何凭证");
        return None;
    }
    match execute(instance_id).await {
        Ok(Some(asset_id)) => {
            tracing::info!(%instance_id, %asset_id, "资产处置已执行（凭证 + 卡片 + 留痕）");
            Some(asset_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                code = error.code,
                msg = %error.msg,
                "资产处置执行失败（审批单不回滚；资产停在原状态，可重新发起处置）"
            );
            None
        }
    }
}

async fn execute(instance_id: Uuid) -> std::result::Result<Option<Uuid>, ApiError> {
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询处置审批单"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(asset_id) = instance.biz_id else {
        return Ok(None);
    };
    let (kind, disposal_date, reason, sale_price, output_tax) = disposal_spec_of(&instance.form_data)?;

    let tx = db().begin().await.map_err(|error| internal(error, "开启处置事务"))?;
    let outcome = async {
        // ① 锁卡（与「本月计提」并发时不能两边都用旧净值算）
        let Some(asset) = asset_model::asset_lock_in(&tx, asset_id)
            .await
            .map_err(|error| internal(error, "锁定资产卡片"))?
        else {
            return Ok::<Option<(Uuid, asset_linkage::AssetStatusChange)>, ApiError>(None);
        };
        if !asset_dto::disposable(&asset.status) {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_DISPOSAL_STATE,
                format!(
                    "资产 {} 当前是「{}」，只有在役 / 闲置的资产能处置",
                    asset.asset_no,
                    status_label(&asset.status)
                ),
            ));
        }
        let vouchers = post_disposal_vouchers(&tx, &asset, kind, disposal_date, sale_price, output_tax).await?;
        let first_voucher = vouchers.first().copied();
        // ② 状态机（唯一入口：校验边 + 写 biz_status_log）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            asset.id,
            &asset.asset_no,
            &asset_bridge_edges(),
            &asset.status,
            asset_dto::STATUS_DISPOSED,
            instance.initiator_id,
            Some(&format!("{}处置：{reason}", kind.label())),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ③ 卡片回填（状态 + 处置三列）
        let affected = asset_model::asset_mark_disposed(
            &tx,
            asset.id,
            kind.as_str(),
            disposal_date,
            first_voucher,
            Some(instance.initiator_id),
        )
        .await
        .map_err(|error| internal(error, "标记资产已处置"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_DISPOSAL_STATE,
                format!("资产 {} 的状态在处置期间已被改动，请刷新后重试", asset.asset_no),
            ));
        }
        // ④ 变动留痕（DISPOSAL，append-only）。返回的变动行 id 就是**本次事件标识**：
        //    通知幂等键要用它（同一张卡「处置 → 作废 → 再处置」的第二次必须能发通知）。
        let event_ref = asset_model::insert_change(
            &tx,
            asset.id,
            &asset_model::ChangeInsert {
                change_type: asset_dto::CHANGE_DISPOSAL,
                change_date: disposal_date,
                from_dept_id: Some(asset.use_dept_id),
                from_dept_name: Some(asset.use_dept_name.as_str()),
                to_dept_id: None,
                to_dept_name: None,
                from_user_id: asset.custodian_id,
                from_user_name: asset.custodian_name.as_deref(),
                to_user_id: None,
                to_user_name: None,
                from_status: Some(asset.status.as_str()),
                to_status: Some(asset_dto::STATUS_DISPOSED),
                reason: &reason,
                voucher_id: first_voucher,
                approval_instance_id: Some(instance_id),
                operator_id: Some(instance.initiator_id),
                operator_name: instance.initiator_name.as_deref(),
                remark: Some(&format!(
                    "{}处置；凭证 {} 张；出售价 {} / 销项税 {}",
                    kind.label(),
                    vouchers.len(),
                    sale_price,
                    output_tax
                )),
            },
        )
        .await
        .map_err(|error| internal(error, "写处置留痕"))?;
        // ⑤ 跨单据联动（事务内动作；处置凭证由本函数 ① 步出 ⇒ 引擎排除
        //    GENERATE_VOUCHER / REVERSE_VOUCHER，见 asset_linkage 文件头）。
        //    失败 → 整笔处置回滚（不会留下「卡已处置、联动没跑」的半截状态）。
        //    `event_ref` = ④ 的变动行 id：进通知幂等键，事件维度去重。
        let change = asset_linkage::run_in_tx(
            &tx,
            asset.id,
            &asset.asset_no,
            asset_dto::STATUS_DISPOSED,
            Some(instance.initiator_id),
            event_ref,
        )
        .await?;
        Ok(Some((asset.id, change)))
    }
    .await;
    match outcome {
        Ok(Some((asset_id, change))) => {
            tx.commit().await.map_err(|error| internal(error, "提交处置事务"))?;
            // 铁律：通知 / 建任务只能在事务提交**之后**发（提交失败 = 处置没做成，
            // 先发通知就是幽灵通知）。
            asset_linkage::run_deferred_after_commit(change).await;
            Ok(Some(asset_id))
        }
        Ok(None) => {
            tx.commit().await.map_err(|error| internal(error, "提交处置事务"))?;
            Ok(None)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 资产状态机合法边（与 `state_machine::FIXED_ASSET_EDGES` 同源）。
pub const fn asset_bridge_edges() -> [(&'static str, &'static str); 5] {
    state_machine::FIXED_ASSET_EDGES
}

/// 处置凭证要用到的科目（1601/1602 取分类上配的，其余按编码取）。
pub async fn disposal_accounts(
    ex: &impl SqlExecutor,
    asset: &AssetRow,
) -> std::result::Result<DisposalAccounts, ApiError> {
    let category = asset_model::category_find_in(ex, asset.category_id)
        .await
        .map_err(|error| internal(error, "查询资产分类"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("资产 {} 的分类已删除，无法确定处置科目", asset.asset_no),
            )
        })?;
    let asset_account = voucher_multi::ensure_leaf(ex, category.asset_account_id)
        .await
        .map_err(|error| voucher_error(error, "原值科目"))?;
    let accum_account = voucher_multi::ensure_leaf(ex, category.accum_account_id)
        .await
        .map_err(|error| voucher_error(error, "累计折旧科目"))?;
    Ok(DisposalAccounts {
        asset: asset_account,
        accum: accum_account,
        clearing: leaf_by_code(ex, "1606", "固定资产清理科目").await?,
        cash: leaf_by_code(ex, "1002", "银行存款科目").await?,
        output_tax: leaf_by_code(ex, "2221.02", "销项税额科目").await?,
        loss: leaf_by_code(ex, "5711", "营业外支出科目").await?,
        income: leaf_by_code(ex, "5301", "营业外收入科目").await?,
        pending: leaf_by_code(ex, "1901", "待处理财产损溢科目").await?,
    })
}

/// 按编码取末级科目（缺失 / 非末级 → 带上下文的 `ApiError`）。
async fn leaf_by_code(
    ex: &impl SqlExecutor,
    code: &'static str,
    what: &'static str,
) -> Result<voucher::AccountRef, ApiError> {
    voucher_multi::leaf_required(ex, code)
        .await
        .map_err(move |error| voucher_error(error, what))
}

/// 生成处置凭证（1~3 张，同一事务）。
pub async fn post_disposal_vouchers(
    ex: &impl SqlExecutor,
    asset: &AssetRow,
    kind: DisposalKind,
    disposal_date: NaiveDate,
    sale_price: Decimal,
    output_tax: Decimal,
) -> std::result::Result<Vec<Uuid>, ApiError> {
    let accounts = disposal_accounts(ex, asset).await?;
    let amounts = DisposalAmounts {
        original: asset.original_value,
        accumulated: asset.accumulated_depreciation,
        net_value: asset.net_value,
        sale_price: asset_compute::money(sale_price),
        output_tax: asset_compute::money(output_tax),
    };
    let specs = asset_compute::disposal_vouchers(kind, &accounts, &amounts);
    let now = Utc::now();
    let mut ids = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let summary = format!(
            "{} {} {}处置（{}/{}）",
            asset.asset_no,
            asset.asset_name,
            kind.label(),
            index + 1,
            specs.len()
        );
        let id = voucher_multi::post_explicit_lines(
            ex,
            SOURCE_DISPOSAL,
            Some(asset.id),
            &asset.asset_no,
            disposal_date,
            &summary,
            &spec.lines,
            Some(spec.label),
            None,
            now,
        )
        .await
        .map_err(|error| voucher_error(error, "生成处置凭证"))?;
        ids.push(id);
    }
    Ok(ids)
}

/// 作废处置：红冲该资产**全部未红冲的蓝字处置凭证**，返回红字凭证 id 列表。
pub async fn reverse_disposal_vouchers(
    ex: &impl SqlExecutor,
    asset_id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<Vec<Uuid>, ApiError> {
    let ids = asset_model::voucher_ids_of_source(ex, SOURCE_DISPOSAL, asset_id, true)
        .await
        .map_err(|error| internal(error, "查询处置凭证"))?;
    let now = Utc::now();
    let mut reds = Vec::new();
    for id in ids {
        let Some(row) = voucher::find_voucher(ex, id)
            .await
            .map_err(|error| internal(error, "查询处置凭证"))?
        else {
            continue;
        };
        if row.reversed_by.is_some() {
            continue; // 已经红冲过（上一次作废处置留下的）
        }
        let red = voucher::reverse_posted(
            ex,
            &row,
            voucher::ReversalInput {
                voucher_date: now.date_naive(),
                reason: Some(reason),
            },
            operator,
            now,
        )
        .await
        .map_err(|error| voucher_error(error, "红冲处置凭证"))?;
        reds.push(red);
    }
    Ok(reds)
}

/// `DbError` → `ApiError`：凭证侧的「期间已结账 / 已红冲」是业务冲突（400/409），
/// 不能一律翻成 500（`utils::db_error` 会把原因藏进 internal_detail，前端只看到「内部错误」）。
pub fn voucher_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::InvalidValue(msg) => {
            let status = if msg.contains("已结账") || msg.contains("已经红冲") {
                409
            } else {
                400
            };
            let code = if status == 409 {
                error_code::ERP_ASSET_PERIOD_CLOSED
            } else {
                error_code::PARAM_INVALID
            };
            ApiError::new(status, code, format!("{ctx}：{msg}"))
        }
        other => internal(other, ctx),
    }
}

fn internal(error: DbError, ctx: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{ctx}失败：{error}"))
}

// ==================== 内控：制单人不能批自己的处置 ====================

/// 内控用：这张资产卡的制单人。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(asset_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 按主键取 1 行的 create_by。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_fixed_asset WHERE id = ?::uuid",
            vec![Value::from(&asset_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("create_by"),
    }
}

/// 审批入口的内控闸（**事务之前**）：发起人 / 制单人不能批自己那一票 → 403 + 65019。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
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
            "内控要求：资产处置的发起人不能审批自己提交的处置（请换一位审批人，或由他人代提）",
        ));
    }
    if let Some(asset_id) = instance.biz_id
        && let Ok(Some(creator)) = internal_control_creator(Some(asset_id)).await
        && creator == operator
    {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：资产卡的制单人不能审批自己的资产处置（请换一位审批人）",
        ));
    }
    Ok(())
}

/// 当前时间（CST 业务日）—— 处置日期 / 折旧目标期间的默认值都用它。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    voucher::today_cst(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 只有 APPROVED 才执行处置（驳回 / 撤回 / 终止都不动账）。
    #[test]
    fn only_approved_executes() {
        assert!(should_execute("APPROVED"));
        assert!(should_execute("approved"));
        assert!(!should_execute("REJECTED"));
        assert!(!should_execute("WITHDRAWN"));
        assert!(!should_execute("CANCELLED"));
        assert!(!should_execute("PENDING"));
    }

    /// ② `form_data` → 处置参数：金额认字符串也认数字。
    #[test]
    fn form_data_parses_both_amount_shapes() {
        let form = serde_json::json!({
            "disposal_type": "SALE",
            "disposal_date": "2026-11-05",
            "reason": "闲置设备出售",
            "sale_price": "50000.0000",
            "output_tax": 6500,
        });
        let (kind, date, reason, price, tax) = disposal_spec_of(&form).expect("合法表单");
        assert_eq!(kind, DisposalKind::Sale);
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 11, 5).unwrap());
        assert_eq!(reason, "闲置设备出售");
        assert_eq!(price, Decimal::new(500_000_000, 4));
        assert_eq!(tax, Decimal::new(65_000_000, 4));

        let broken = serde_json::json!({ "disposal_type": "DONATE", "disposal_date": "2026-11-05" });
        assert!(disposal_spec_of(&broken).is_err(), "非法处置类型必须报错而不是猜");
    }

    /// ③ 合法边表与状态机注册表逐条一致（防两处漂移）。
    #[test]
    fn edges_match_registry() {
        assert_eq!(
            asset_bridge_edges().as_slice(),
            state_machine::Entity::FixedAsset.edges(),
            "asset_bridge 的边表必须与状态机注册表完全一致"
        );
    }

    /// ④ 处置凭证的期间是处置日所在期间（决定能不能落账）。
    #[test]
    fn disposal_period_follows_date() {
        let last_day = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        assert_eq!(asset_compute::period_of(last_day), "2026-11");
        assert_eq!(
            asset_compute::period_of(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
            "2026-11"
        );
    }
}
