//! 折旧计提的数据层与批跑口径（ARCH-6 §5.4 / §5.5）。
//!
//! # 一次计提 = 一个事务里的五步
//!
//! ```text
//! ① 期间闸：CLOSED → 拒绝（409 + 66038）；NOT_OPENED / 无期间行 → 拒绝（提示先开账）
//! ② 幂等闸：该期间已有批次 → 直接返回（POSTED / CALCULATED / SKIPPED 都不重提）
//! ③ 扫描在役资产 → 逐台算本月应提额（纯函数 `asset_compute::month_amount`）
//! ④ 落明细 → 按（费用科目, 部门）聚合生成**一张多借方凭证** / 贷 1602 汇总
//! ⑤ 回写卡片缓存列 + 批次置 POSTED
//! ```
//!
//! # 为什么期间已结账时**不落 SKIPPED 批次行**
//!
//! 设计 §5.5.1 的流程图里，CLOSED / NOT_OPENED 会写一条 `SKIPPED` 批次行。
//! 实施时发现这与 §5.2.4 的 `uk_biz_dep_batch_period`（一个期间只能有一批）
//! 冲突：一旦因为这个原因写了一条 SKIPPED 行，**反结账之后再跑就永久撞唯一键**
//! （那一期永远提不出折旧）。所以本实现改成：
//!
//! * 「期间不可写」→ **不落行**，直接 409 拒绝（`ERP_ASSET_PERIOD_CLOSED`），日志 WARN；
//! * `SKIPPED` 只用于**合法空跑**（本期确实没有应提折旧：没有在役资产 / 全部提足）。
//!
//! 差异写进证据 README（D-2）。
//!
//! # 红冲后重跑（§5.5.5）
//!
//! `REVERSED → CALCULATED` 复用**同一批次行**（不能新建：`period_code` 唯一），
//! 明细整批删掉重算，`reverse_voucher_id` 保留上一次的红字凭证指针（历史链靠凭证的
//! `reverse_of / reversed_by` 追溯）。

use std::collections::BTreeMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::asset_compute::{self, DepreciationCard};
use crate::business::erp::asset_dto::{self, DepRunKind, DepRunOutcome, STATUS_DISPOSED};
use crate::business::erp::asset_model::{self, DepBatchRow, DepDetailRow, DepInsert};
use crate::business::erp::depreciation_linkage;
use crate::business::erp::voucher;
use crate::business::erp::voucher_multi;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 折旧凭证的来源类型。
pub const SOURCE_DEPRECIATION: &str = "DEPRECIATION";
/// 折旧凭证的规则码（`biz_voucher_rule.rule_code`，V0232 种子）。
pub const RULE_CODE: &str = "DEPRECIATION_DEFAULT";
/// 期间状态：已结账。
pub const PERIOD_CLOSED: &str = "CLOSED";

/// 一条借方聚合（同一费用科目 + 同一部门 = 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebitAgg {
    /// 费用科目 id。
    pub account_id: Uuid,
    /// 费用科目编码（日志与断言用）。
    pub account_code: String,
    /// 部门 id（写 `biz_voucher_entry.aux_dept_id`）。
    pub dept_id: Uuid,
    /// 部门名（摘要用）。
    pub dept_name: String,
    /// 合计金额。
    pub amount: Decimal,
}

/// 按（费用科目, 部门）聚合借方（ARCH-6 §5.5.3 的「拆不拆」口径）。
///
/// ```text
/// 同一费用科目 + 同一部门 → 合并一行（凭证语义单位是「科目 + 辅助维度」，不是资产）
/// 不同费用科目            → 拆行
/// 不同部门                → 拆行
/// ```
///
/// 排序固定 `(account_code, dept_name)`：凭证分录的行序必须可复现，
/// 否则同一批数据重跑出来的凭证与红冲凭证行序不一致，人工对账会怀疑漏行。
pub fn aggregate_debits(details: &[DepInsert]) -> Vec<DebitAgg> {
    let mut map: BTreeMap<(String, String), DebitAgg> = BTreeMap::new();
    for detail in details {
        let key = (
            detail.expense_account_code.clone(),
            detail.dept_name.clone(),
        );
        let entry = map.entry(key).or_insert_with(|| DebitAgg {
            account_id: detail.expense_account_id,
            account_code: detail.expense_account_code.clone(),
            dept_id: detail.dept_id,
            dept_name: detail.dept_name.clone(),
            amount: Decimal::ZERO,
        });
        entry.amount += detail.amount;
    }
    map.into_values().collect()
}

/// 逐资产算出本期折旧明细（纯函数：给卡片与分类，返回待写明细）。
///
/// 返回 `None` = 这台本期不提（提足 / 金额为 0 / 未到起提日）。
pub fn compute_detail(
    asset: &asset_model::AssetRow,
    expense_account_id: Uuid,
    expense_account_code: &str,
    period_code: &str,
) -> Option<DepInsert> {
    // 规则①：当月增加当月不提（起提日 > 期末 的不在扫描结果里，这里再判一次是为了
    // 「直接调用 compute_detail 的用例」也能自证）
    if let Some((_, period_end)) = asset_compute::period_range(period_code)
        && asset.start_depreciation_date > period_end
    {
        return None;
    }
    let card = DepreciationCard {
        original_value: asset.original_value,
        residual_value: asset.residual_value,
        depreciable_base: asset.depreciable_base,
        monthly_depreciation: asset.monthly_depreciation,
        useful_life_months: asset.useful_life_months,
        accumulated_depreciation: asset.accumulated_depreciation,
        months_done: asset.depreciation_months_done,
    };
    let amount = asset_compute::month_amount(&card)?;
    let accumulated_after = asset.accumulated_depreciation + amount;
    let net_value_after = asset.original_value - accumulated_after;
    Some(DepInsert {
        asset_id: asset.id,
        asset_no: asset.asset_no.clone(),
        asset_name: asset.asset_name.clone(),
        dept_id: asset.use_dept_id,
        dept_name: asset.use_dept_name.clone(),
        expense_account_id,
        expense_account_code: expense_account_code.to_string(),
        amount,
        accumulated_before: asset.accumulated_depreciation,
        accumulated_after,
        net_value_after,
        is_last_month: asset_compute::is_last_month(&card),
    })
}

/// 计提（手动 / 定时任务共用）。`period_code = None` → 上一个会计月。
pub async fn run_batch(
    period_code: Option<&str>,
    operator: Option<Uuid>,
    remark: Option<&str>,
) -> std::result::Result<DepRunOutcome, ApiError> {
    let now = Utc::now();
    let today = voucher::today_cst(now);
    let period = match period_code.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            if !asset_compute::is_valid_period(text) {
                return Err(asset_dto::invalid(format!(
                    "计提期间必须是 yyyy-MM（当前 {text}）"
                )));
            }
            text.to_string()
        }
        // 定时任务口径（ARCH-6 D7 §2.7.1）：每月 1 日跑【上一个会计月】
        None => asset_compute::prev_period(&asset_compute::period_of(today))
            .ok_or_else(|| asset_dto::invalid("无法从今天推导上一个会计月（日期异常）"))?,
    };
    let Some((period_start, period_end)) = asset_compute::period_range(&period) else {
        return Err(asset_dto::invalid(format!("期间 {period} 不合法")));
    };

    // ① 期间闸
    match asset_model::period_status_of(&period)
        .await
        .map_err(|error| db_err(error, "查询期间状态"))?
    {
        None => {
            tracing::warn!(period = %period, "折旧计提被拒绝：该期间还没有期间记录（未开账）");
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_PERIOD_CLOSED,
                format!(
                    "会计期间 {period} 还没有开账：请先在「会计期间」里开账再计提折旧\
                     （折旧凭证要落在这一期，没有期间行就落不了账）"
                ),
            ));
        }
        Some(status) if status == PERIOD_CLOSED => {
            tracing::warn!(period = %period, "折旧计提被拒绝：期间已结账（补提需先反结账）");
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_PERIOD_CLOSED,
                format!(
                    "会计期间 {period} 已结账，不能再补提折旧：已封账的月份只读。\
                     要补提请先反结账（反结账是人工动作，需要填至少 10 个字的原因），再重跑本期"
                ),
            ));
        }
        Some(status) if status != voucher::PERIOD_OPEN => {
            tracing::warn!(period = %period, status = %status, "折旧计提被拒绝：期间未开启");
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_PERIOD_CLOSED,
                format!(
                    "会计期间 {period} 当前是「{status}」：还没开账，折旧凭证落不进去。\
                     请先在「会计期间」里开账再计提"
                ),
            ));
        }
        Some(_) => {}
    }

    // ② 幂等闸
    let existing = asset_model::batch_by_period(db(), &period)
        .await
        .map_err(|error| db_err(error, "查询折旧批次"))?;
    let rerun = match &existing {
        None => false,
        Some(batch) if batch.status == asset_dto::DEP_REVERSED => true,
        Some(batch) => {
            return Ok(DepRunOutcome {
                outcome: DepRunKind::Exists,
                total_amount: batch.total_amount,
                asset_count: batch.asset_count,
                message: format!(
                    "期间 {period} 已有批次 {}（状态 {}）：不重复计提{}",
                    batch.batch_no,
                    batch.status,
                    if batch.status == asset_dto::DEP_SKIPPED {
                        format!(
                            "（跳过原因：{}）",
                            batch.skip_reason.clone().unwrap_or_default()
                        )
                    } else {
                        String::new()
                    }
                ),
                batch: Some(asset_dto::DepBatchRes::from(batch)),
            });
        }
    };

    // ③ 扫描 + 逐台计算
    let assets = asset_model::scan_assets_for_period(db(), &period, period_start, period_end)
        .await
        .map_err(|error| db_err(error, "扫描在役资产"))?;
    let categories: BTreeMap<Uuid, asset_model::CategoryRow> = asset_model::category_active()
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let mut details: Vec<DepInsert> = Vec::with_capacity(assets.len());
    let mut credit_account_id: Option<Uuid> = None;
    for asset in &assets {
        let Some(category) = categories.get(&asset.category_id) else {
            // 分类被停用 / 软删：这一台本期不提，日志留痕（不阻断整批）
            tracing::warn!(
                asset_no = %asset.asset_no,
                category_id = %asset.category_id,
                "资产分类不可用（已停用 / 已删除），本期跳过这台资产"
            );
            continue;
        };
        // 费用科目编码（凭证与明细都要快照它）
        let code = account_code_of(category.expense_account_id).await?;
        if let Some(detail) = compute_detail(asset, category.expense_account_id, &code, &period) {
            // 贷方 1602 取「第一台被计提资产」的分类快照（§5.5.3：贷方汇总一行）
            credit_account_id.get_or_insert(category.accum_account_id);
            details.push(detail);
        }
    }
    let total: Decimal = details.iter().map(|detail| detail.amount).sum();

    // ④ 合法空跑 → SKIPPED 批次（本期无应提折旧）
    if details.is_empty() {
        let batch_no = asset_model::next_batch_no(db(), now)
            .await
            .map_err(|error| db_err(error, "取折旧批次号"))?;
        let id = Uuid::now_v7();
        asset_model::batch_insert(
            db(),
            id,
            &batch_no,
            &period,
            0,
            Decimal::ZERO,
            operator,
            remark,
        )
        .await
        .map_err(|error| db_err(error, "写折旧批次"))?;
        asset_model::batch_set_status(
            db(),
            id,
            asset_dto::DEP_CALCULATED,
            asset_dto::DEP_SKIPPED,
            None,
            None,
            Some("本期无应提折旧（没有在役未提足的资产）"),
            now,
        )
        .await
        .map_err(|error| db_err(error, "标记批次跳过"))?;
        let batch = asset_model::batch_find(id)
            .await
            .map_err(|error| db_err(error, "查询折旧批次"))?;
        tracing::info!(period = %period, batch_no = %batch_no, "折旧计提：本期无应提折旧，批次跳过");
        return Ok(DepRunOutcome {
            outcome: DepRunKind::Skipped,
            total_amount: Decimal::ZERO,
            asset_count: 0,
            message: format!(
                "期间 {period} 没有应提折旧的资产（全部提足 / 未到起提日），批次已跳过"
            ),
            batch: batch.as_ref().map(asset_dto::DepBatchRes::from),
        });
    }

    // ⑤ 一个事务里：批次 + 明细 + 凭证 + 回写卡片
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启计提事务"))?;
    let outcome = async {
        let (batch_id, batch_no) = match existing.as_ref() {
            Some(batch) if rerun => {
                // 红冲后重跑：复用同一行（uk_biz_dep_batch_period 挡着不能新建）
                state_machine::transition(
                    &tx,
                    state_machine::ENTITY_DEPRECIATION,
                    batch.id,
                    &batch.period_code,
                    &state_machine::DEPRECIATION_EDGES,
                    asset_dto::DEP_REVERSED,
                    asset_dto::DEP_CALCULATED,
                    operator,
                    Some("红冲后重跑（复用同一批次行）"),
                    None,
                )
                .await?;
                asset_model::batch_clear_details(&tx, batch.id)
                    .await
                    .map_err(|error| db_err(error, "清理上一轮明细"))?;
                asset_model::batch_set_status(
                    &tx,
                    batch.id,
                    asset_dto::DEP_REVERSED,
                    asset_dto::DEP_CALCULATED,
                    None,
                    None,
                    None,
                    now,
                )
                .await
                .map_err(|error| db_err(error, "批次回到已计算"))?;
                asset_model::batch_reset_for_rerun(
                    &tx,
                    batch.id,
                    i32::try_from(details.len()).unwrap_or(0),
                    total,
                    remark,
                )
                .await
                .map_err(|error| db_err(error, "重置批次金额"))?;
                (batch.id, batch.batch_no.clone())
            }
            _ => {
                let batch_no = asset_model::next_batch_no(&tx, now)
                    .await
                    .map_err(|error| db_err(error, "取折旧批次号"))?;
                let id = Uuid::now_v7();
                asset_model::batch_insert(
                    &tx,
                    id,
                    &batch_no,
                    &period,
                    i32::try_from(details.len()).unwrap_or(0),
                    total,
                    operator,
                    remark,
                )
                .await
                .map_err(|error| db_err(error, "写折旧批次"))?;
                (id, batch_no)
            }
        };

        // 明细
        for detail in &details {
            asset_model::insert_depreciation(&tx, batch_id, &period, detail)
                .await
                .map_err(|error| db_err(error, "写折旧明细"))?;
        }

        // 凭证（多借方：按费用科目 + 部门聚合）
        let aggs = aggregate_debits(&details);
        let credit_account_id = credit_account_id.ok_or_else(|| {
            ApiError::new(
                409,
                error_code::ERP_DEPRECIATION_STATE,
                "本期没有可计提的明细，不能生成折旧凭证",
            )
        })?;
        let credit_account = voucher_multi::ensure_leaf(&tx, credit_account_id)
            .await
            .map_err(|error| voucher_error(error, "解析累计折旧科目"))?;
        let mut debits = Vec::with_capacity(aggs.len());
        for agg in &aggs {
            let account = voucher_multi::ensure_leaf(&tx, agg.account_id)
                .await
                .map_err(|error| voucher_error(error, "解析折旧费用科目"))?;
            debits.push(
                voucher_multi::DebitGroup::new(account, agg.amount, Some(agg.dept_id))
                    .with_summary(format!("{period} 折旧（{}）", agg.dept_name)),
            );
        }
        let batch_period = period.clone();
        let voucher_id = voucher_multi::post_multi_debit(
            &tx,
            &voucher_multi::MultiDebit {
                source_type: SOURCE_DEPRECIATION,
                source_id: Some(batch_id),
                source_no: &batch_period,
                voucher_date: period_end,
                summary: format!("{period} 计提固定资产折旧"),
                debits,
                credit_account,
                credit_amount: total,
                credit_summary: Some(format!("{period} 计提折旧汇总")),
                remark,
            },
            operator,
            now,
        )
        .await
        .map_err(|error| voucher_error(error, "生成折旧凭证"))?;

        // 回写卡片（缓存列的唯一写入者）
        for detail in &details {
            asset_model::apply_depreciation_to_asset(
                &tx,
                detail.asset_id,
                detail.amount,
                detail.accumulated_after,
                detail.net_value_after,
                &period,
            )
            .await
            .map_err(|error| db_err(error, "回写卡片累计折旧"))?;
        }

        // 批次过账
        let affected = asset_model::batch_set_status(
            &tx,
            batch_id,
            asset_dto::DEP_CALCULATED,
            asset_dto::DEP_POSTED,
            Some(voucher_id),
            None,
            None,
            now,
        )
        .await
        .map_err(|error| db_err(error, "批次置已过账"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_DEPRECIATION_STATE,
                "折旧批次状态在计提期间被改动，请刷新后重试",
            ));
        }

        // ⑤' 联动（LNK-10）：批次已置 POSTED，事务内动作在这里跑，通知等 commit 之后。
        // 触发单据是**批次**（不是每台资产）：300 台资产也只跑一遍规则、只发一条通知；
        // 凭证由第 ④ 步 `post_multi_debit` 自己出，所以传
        // `DEPRECIATION_EXCLUDED_ACTIONS`（GENERATE_VOUCHER / REVERSE_VOUCHER 交给专用路径）。
        // `event_ref = voucher_id`：同一笔过账重放只发一条，红冲后重跑（新凭证）会再发一条。
        let change = depreciation_linkage::run_in_tx(
            &tx,
            batch_id,
            &batch_no,
            &period,
            asset_dto::DEP_POSTED,
            i32::try_from(details.len()).unwrap_or(0),
            voucher_id,
            operator,
        )
        .await?;
        Ok::<(Uuid, Uuid, depreciation_linkage::DepBatchStatusChange), ApiError>((
            batch_id, voucher_id, change,
        ))
    }
    .await;

    let (batch_id, voucher_id, change) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| db_err(error, "提交计提事务"))?;
    // 提交之后的联动动作（通知 / 建任务）：批次是大批量长事务，必须在 commit 后才发 ——
    // 事务内发通知一旦回滚就是「财务收到某月已计提、实际没提」
    depreciation_linkage::run_deferred_after_commit(change).await;
    let batch = asset_model::batch_find(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧批次"))?;
    tracing::info!(
        period = %period,
        batch_id = %batch_id,
        %voucher_id,
        asset_count = details.len(),
        total = %total,
        "折旧计提完成（POSTED）"
    );
    Ok(DepRunOutcome {
        outcome: if rerun {
            DepRunKind::ReversedRerun
        } else {
            DepRunKind::Posted
        },
        total_amount: total,
        asset_count: i32::try_from(details.len()).unwrap_or(0),
        message: if rerun {
            format!(
                "期间 {period} 红冲后重跑完成：{} 台 / 合计 {total}（复用原批次行）",
                details.len()
            )
        } else {
            format!(
                "期间 {period} 计提完成：{} 台 / 合计 {total}",
                details.len()
            )
        },
        batch: batch.as_ref().map(asset_dto::DepBatchRes::from),
    })
}

/// 科目编码（明细快照用）。
async fn account_code_of(account_id: Uuid) -> std::result::Result<String, ApiError> {
    // SCOPE-EXEMPT: 科目表按主键取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT account_code FROM biz_account WHERE id = ?::uuid",
            vec![Value::from(&account_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询会计科目"))?;
    match rows.first() {
        Some(row) => row
            .get_typed("account_code")
            .map_err(|error| db_err(error, "读取科目编码")),
        None => Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("折旧费用科目 {account_id} 不存在：请先修好资产分类的科目配置"),
        )),
    }
}

/// 红冲一个折旧批次（ARCH-6 §5.5.5 的六步，同一事务）。
pub async fn reverse_batch(
    batch_id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<DepBatchRow, ApiError> {
    let reason = reason.trim();
    if reason.chars().count() < 2 {
        return Err(asset_dto::invalid(
            "红冲原因必填（至少 2 个字）：红冲是财务动作，必须说清为什么",
        ));
    }
    let Some(batch) = asset_model::batch_find(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧批次"))?
    else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "折旧批次不存在",
        ));
    };
    if batch.status != asset_dto::DEP_POSTED {
        return Err(ApiError::new(
            409,
            error_code::ERP_DEPRECIATION_STATE,
            format!(
                "折旧批次 {} 当前是「{}」：只有已过账（POSTED）的批次能红冲",
                batch.batch_no, batch.status
            ),
        ));
    }
    // 期间闸（§5.5.5 ①：已结账 → 409，必须先反结账）
    if voucher::is_period_closed(db(), &batch.period_code)
        .await
        .map_err(|error| db_err(error, "查询期间状态"))?
    {
        return Err(ApiError::new(
            409,
            error_code::ERP_ASSET_PERIOD_CLOSED,
            format!(
                "会计期间 {} 已结账，不能红冲该批次的折旧（请先反结账）",
                batch.period_code
            ),
        ));
    }
    let details = asset_model::details_of(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧明细"))?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启红冲事务"))?;
    let outcome = async {
        // ② 红字凭证（借贷互换、金额恒正）
        let ids = asset_model::voucher_ids_of_source(&tx, SOURCE_DEPRECIATION, batch_id, true)
            .await
            .map_err(|error| db_err(error, "查询折旧凭证"))?;
        let mut reds = Vec::new();
        for id in ids {
            let Some(row) = voucher::find_voucher(&tx, id)
                .await
                .map_err(|error| db_err(error, "查询折旧凭证"))?
            else {
                continue;
            };
            if row.reversed_by.is_some() {
                continue;
            }
            let red = voucher::reverse_posted(
                &tx,
                &row,
                voucher::ReversalInput {
                    voucher_date: now.date_naive(),
                    reason: Some(reason),
                },
                operator,
                now,
            )
            .await
            .map_err(|error| voucher_error(error, "红冲折旧凭证"))?;
            reds.push(red);
        }
        if reds.is_empty() {
            return Err(ApiError::new(
                409,
                error_code::ERP_DEPRECIATION_STATE,
                format!("折旧批次 {} 的凭证找不到或已经红冲过", batch.batch_no),
            ));
        }
        // ③ 明细标 REVERSED
        asset_model::reverse_details(&tx, batch_id)
            .await
            .map_err(|error| db_err(error, "标记明细已红冲"))?;
        // ④ 卡片回滚（累计减、净值加、月数减、最后期间回退）
        for detail in details.iter().filter(|detail| detail.status == "POSTED") {
            asset_model::rollback_depreciation_on_asset(&tx, detail.asset_id, detail.amount)
                .await
                .map_err(|error| db_err(error, "回滚卡片累计折旧"))?;
        }
        // ⑤ 批次 REVERSED
        state_machine::transition(
            &tx,
            state_machine::ENTITY_DEPRECIATION,
            batch.id,
            &batch.period_code,
            &state_machine::DEPRECIATION_EDGES,
            asset_dto::DEP_POSTED,
            asset_dto::DEP_REVERSED,
            operator,
            Some(reason),
            None,
        )
        .await?;
        let last_red = reds.last().copied();
        let affected = asset_model::batch_set_status(
            &tx,
            batch_id,
            asset_dto::DEP_POSTED,
            asset_dto::DEP_REVERSED,
            None,
            last_red,
            None,
            now,
        )
        .await
        .map_err(|error| db_err(error, "批次置已红冲"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_DEPRECIATION_STATE,
                "折旧批次状态在红冲期间被改动，请刷新后重试",
            ));
        }

        // ⑥ 联动（LNK-10）：批次已置 REVERSED，事务内动作在这里跑，通知等 commit 之后。
        // 触发单据同样是**批次**：几百条明细也只跑一遍规则、只发一条通知；
        // 红字凭证由第 ② 步 `reverse_posted` 自己出，所以传
        // `DEPRECIATION_EXCLUDED_ACTIONS`（GENERATE_VOUCHER / REVERSE_VOUCHER 交给专用路径）。
        // `event_ref = 红字凭证 id`：同一笔红冲重放只发一条，重跑再红冲（新凭证）会再发一条。
        let change = depreciation_linkage::run_in_tx(
            &tx,
            batch_id,
            &batch.batch_no,
            &batch.period_code,
            asset_dto::DEP_REVERSED,
            i32::try_from(details.len()).unwrap_or(0),
            reds.last().copied().unwrap_or_else(Uuid::now_v7),
            operator,
        )
        .await?;
        Ok::<depreciation_linkage::DepBatchStatusChange, ApiError>(change)
    }
    .await;
    let change = match outcome {
        Ok(change) => change,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| db_err(error, "提交红冲事务"))?;
    // 提交之后的联动动作（通知 / 建任务）：红冲同样是长事务（红字凭证 + 逐台回滚卡片），
    // 必须在 commit 后才发 —— 事务内发通知一旦回滚就是「财务收到某月已红冲、实际没冲」
    depreciation_linkage::run_deferred_after_commit(change).await;
    tracing::info!(%batch_id, period = %batch.period_code, "折旧批次已红冲（凭证 + 卡片回滚）");
    asset_model::batch_find(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧批次"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "折旧批次不存在"))
}

/// 资产是否还在「会被折旧扫描到」的状态（红冲后重新纳入 / 处置后排除）。
pub fn in_depreciation_scope(status: &str) -> bool {
    status != STATUS_DISPOSED && asset_dto::disposable(status)
}

/// 批次详情（批次 + 明细 + 合计）。
pub async fn batch_detail(
    batch_id: Uuid,
) -> std::result::Result<asset_dto::DepBatchDetailRes, ApiError> {
    let Some(batch) = asset_model::batch_find(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧批次"))?
    else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "折旧批次不存在",
        ));
    };
    let details = asset_model::details_of(batch_id)
        .await
        .map_err(|error| db_err(error, "查询折旧明细"))?;
    let detail_total: Decimal = details.iter().map(|detail| detail.amount).sum();
    Ok(asset_dto::DepBatchDetailRes {
        next_statuses: state_machine::Entity::Depreciation
            .next_statuses(&batch.status)
            .into_iter()
            .map(str::to_string)
            .collect(),
        batch: asset_dto::DepBatchRes::from(&batch),
        details: details.iter().map(asset_dto::DepDetailRes::from).collect(),
        detail_total,
    })
}

/// 明细行 → 摘要（手动触发端点返回给前端的一行文案）。
pub fn detail_caption(row: &DepDetailRow) -> String {
    format!(
        "{} {} 计提 {}（{} → {}）",
        row.period_code, row.asset_no, row.amount, row.accumulated_before, row.accumulated_after
    )
}

/// 定时任务入口（`erp_depreciation_run`，每月 1 日 01:30 计提上一个会计月）。
///
/// 与手动计提**同一个** `run_batch`：定时任务不是第二条代码路径，
/// 只是「谁来点这个按钮」不同（ARCH-6 §5.5.1 的时序图）。
pub async fn run_scheduled(now: DateTime<Utc>) -> std::result::Result<DepRunOutcome, ApiError> {
    let today = voucher::today_cst(now);
    let period = asset_compute::prev_period(&asset_compute::period_of(today));
    run_batch(period.as_deref(), None, Some("定时任务：计提上一个会计月")).await
}

/// 注册定时任务（在 `task::Scheduler::register_builtin` 里调用）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        "erp_depreciation_run",
        "0 30 1 1 * *",
        "ERP 折旧计提：每月 1 日 01:30 计提上一个会计月的折旧（多借方凭证 + 回写卡片）",
        || {
            tokio::spawn(async move {
                match run_scheduled(Utc::now()).await {
                    Ok(report) => tracing::info!(
                        outcome = %report.outcome.as_str(),
                        asset_count = report.asset_count,
                        total = %report.total_amount,
                        message = %report.message,
                        "折旧计提任务完成"
                    ),
                    Err(error) => tracing::warn!(
                        code = error.code,
                        msg = %error.msg,
                        "折旧计提任务未完成（期间未开账 / 已结账 / 无科目等，不影响服务）"
                    ),
                }
            })
        },
    );
    tracing::info!(
        invoke_target = "erp_depreciation_run",
        cron = "0 30 1 1 * *",
        "折旧计提任务已注册"
    );
}

fn db_err(error: DbError, ctx: &str) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("{ctx}失败：{error}"),
    )
}

fn voucher_error(error: DbError, ctx: &str) -> ApiError {
    crate::business::erp::asset_bridge::voucher_error(error, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金额字面量（同上：自己造 `dec!`，不引 `rust_decimal_macros`）。
    macro_rules! dec {
        ($v:literal) => {{
            use std::str::FromStr as _;
            Decimal::from_str(stringify!($v)).expect("金额字面量非法")
        }};
    }

    fn detail(code: &str, dept: &str, amount: Decimal) -> DepInsert {
        DepInsert {
            asset_id: Uuid::now_v7(),
            asset_no: "FA-1".to_string(),
            asset_name: "设备".to_string(),
            dept_id: Uuid::now_v7(),
            dept_name: dept.to_string(),
            expense_account_id: Uuid::now_v7(),
            expense_account_code: code.to_string(),
            amount,
            accumulated_before: Decimal::ZERO,
            accumulated_after: amount,
            net_value_after: dec!(100000) - amount,
            is_last_month: false,
        }
    }

    /// ① 同科目同部门合并一行；不同部门 / 不同科目拆行（§5.5.3 的口径表）。
    #[test]
    fn aggregate_merges_same_account_and_dept() {
        let mut a = detail("5602.01", "研发部", dec!(100));
        let b = DepInsert {
            expense_account_id: a.expense_account_id,
            dept_id: a.dept_id,
            ..detail("5602.01", "研发部", dec!(23.5))
        };
        let c = detail("5602.01", "产品部", dec!(50));
        let d = detail("4101", "研发部", dec!(7));
        let aggs = aggregate_debits(&[a.clone(), b, c, d]);
        assert_eq!(aggs.len(), 3, "同科目同部门合并 → 三条聚合");
        assert_eq!(aggs[0].account_code, "4101", "按科目编码排序");
        let dev = aggs
            .iter()
            .find(|agg| agg.account_code == "5602.01" && agg.dept_name == "研发部")
            .expect("研发部一行");
        assert_eq!(dev.amount, dec!(123.5), "同组合计");
        // 修改 a 不影响聚合结果（值语义）
        a.amount = dec!(999);
        assert_eq!(aggs.len(), 3);
    }

    /// ② 只处理在役 / 闲置（已处置不进折旧范围）。
    #[test]
    fn scope_excludes_disposed() {
        assert!(in_depreciation_scope("IN_USE"));
        assert!(in_depreciation_scope("IDLE"));
        assert!(!in_depreciation_scope("DISPOSED"));
        assert!(!in_depreciation_scope("CANCELLED"));
    }

    /// ③ 逐台计算：第 1 月按固定额，最后一月补齐尾差。
    #[test]
    fn compute_detail_uses_tail_fill() {
        let asset = asset_model::AssetRow {
            id: Uuid::now_v7(),
            asset_no: "FA-2026-01".to_string(),
            asset_name: "笔记本".to_string(),
            category_id: Uuid::now_v7(),
            category_code: "ELEC".to_string(),
            spec: None,
            use_dept_id: Uuid::now_v7(),
            use_dept_name: "研发部".to_string(),
            custodian_id: None,
            custodian_name: None,
            location: None,
            original_value: dec!(100000),
            tax_amount: Decimal::ZERO,
            residual_value: dec!(5000),
            depreciable_base: dec!(95000),
            depreciation_method: "STRAIGHT_LINE".to_string(),
            useful_life_months: 60,
            salvage_rate: dec!(0.05),
            monthly_depreciation: dec!(1583.3333),
            start_depreciation_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap(),
            accumulated_depreciation: dec!(93416.6647),
            net_value: dec!(6583.3353),
            depreciation_months_done: 58,
            last_depreciation_period: Some("2026-11".to_string()),
            purchase_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
            source_type: "MANUAL".to_string(),
            source_id: None,
            source_no: None,
            buy_mode: Some("DIRECT_BUY".to_string()),
            supplier_id: None,
            supplier_name: None,
            voucher_id: None,
            status: "IN_USE".to_string(),
            disposal_type: None,
            disposal_date: None,
            disposal_voucher_id: None,
            remark: None,
            created_at: Utc::now(),
        };
        let account = Uuid::now_v7();
        let first = compute_detail(&asset, account, "5602.01", "2026-12").expect("第 59 月仍计提");
        assert_eq!(first.amount, dec!(1583.3333));
        assert!(!first.is_last_month);

        let last = compute_detail(
            &asset_model::AssetRow {
                accumulated_depreciation: dec!(93416.6647),
                depreciation_months_done: 59,
                ..asset
            },
            account,
            "5602.01",
            "2027-01",
        )
        .expect("第 60 月补齐尾差");
        assert_eq!(last.amount, dec!(1583.3353));
        assert!(last.is_last_month);
        assert_eq!(last.accumulated_after, dec!(95000.0000));
        assert_eq!(last.net_value_after, dec!(5000.0000), "提足后净值 = 残值");
    }
}
