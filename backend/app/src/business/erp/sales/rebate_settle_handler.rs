//! 销售返利结算接口层（M3-16）。前缀 `/api` 由 `[router].prefix` 注入，宏内写
//! `/business/erp/rebate-settlement/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/rebate-settlement/preview` | `business:erp:rebate-settlement:calc` | 试算（**不落库**：基数 + 落档 + 应返金额 + 已有结算单回显） |
//! | POST | `/business/erp/rebate-settlement` | `business:erp:rebate-settlement:calc` | 计算并落库（`DRAFT` + 政策快照） |
//! | POST | `/business/erp/rebate-settlement/run` | `business:erp:rebate-settlement:calc` | 整期跑批（逐客户一个事务；已有则跳过） |
//! | POST | `/business/erp/rebate-settlement/{id}/recalc` | `business:erp:rebate-settlement:calc` | 重算（仅 `DRAFT`；换政策/新增订单后刷新） |
//! | GET | `/business/erp/rebate-settlement/page` | `business:erp:rebate-settlement:list` | 分页（关键字 / 客户 / 粒度 / 期间 / 状态 / 政策） |
//! | GET | `/business/erp/rebate-settlement/{id}` | `business:erp:rebate-settlement:list` | 详情（含政策全量档位快照） |
//! | DELETE | `/business/erp/rebate-settlement/{id}` | `business:erp:rebate-settlement:delete` | 作废（仅 `DRAFT`；行保留，同期间可重开） |
//!
//! # 本卡只算钱，不转账
//!
//! 「生成应付 / 红字发票」是 **M3-17**。本卡交付到「结算单 `DRAFT` + 政策快照」为止：
//! `SETTLED` 由 M3-17 在生成应付/红字发票后置位（本卡只读不写）。
//!
//! # 五道业务闸
//!
//! | 闸 | 在哪 | 拦什么 |
//! |----|------|--------|
//! | ① 参数形状 | [`PeriodArgs::resolve`] / `parse_uuid` | 期间码不是 `yyyy-MM`/`yyyy-QN`、粒度与期间码不互配、客户 id 非 UUID |
//! | ② 客户可用 | [`require_active_customer`] | 客户不存在 / 已删 → 404；`FROZEN` → 409（冻结客户不再产生新返利） |
//! | ③ 政策覆盖整期 | [`load_policy`] | 期间首日没有启用政策、或政策没覆盖整期 → 409 + `66091`（报文写清缺哪段日期） |
//! | ④ 期间不重复 | [`settle_inner`] | 同客户同粒度同期间已有未作废结算单 → 409 + `66092`（库层 `uk_biz_srs_period` 兜底） |
//! | ⑤ 状态可改 | [`recalc_of`] / [`cancel_of`] | 只有 `DRAFT` 能重算/作废；`SETTLED` → 409（红冲/调账是另一回事） |
//!
//! # 为什么结算取政策要「覆盖整期」而不是「期间内某天生效」
//!
//! 政策 11-15 生效、期间是 11 月全月时：全月套新政策 = 前 14 天不该给也算（超额返利）；
//! 全月不算 = 后 16 天该给的不给。两个都是有争议的数。所以本卡**不算**并报 `66091`，
//! 让人去补政策 —— 结算金额不猜（同一条理由写在 `rebate_settle_dto` 模块头）。
//!
//! # 为什么跑批逐客户一个事务
//!
//! 「A 客户算成了、B 客户算挂了」时，整批回滚会让人无从下手（不知道挂的是谁），
//! 而全部提交又可能把 A 的错数留在库里。逐客户一个事务 + 逐客户结果（`CREATED` /
//! `EXISTS` / `FAILED` + 原因）最贴近财务的实际操作：跑一遍，看哪家失败，修好再跑一遍
//! （幂等：已有结算单的客户返回 `EXISTS`，不会重复算）。
//!
//! # 错误口径
//!
//! | 场景 | 返回 |
//! |------|------|
//! | 参数非法（UUID / 期间码 / 粒度 / 备注超长） | 400 + 10001 |
//! | 政策档位与基数对不上（绕过接口改歪） | 400 + **66089** |
//! | 客户 / 结算单不存在 | 404 + 10002 |
//! | 客户冻结、期间无政策（未覆盖整期）、期间重复、非 `DRAFT` 想改 | 409 + 10003 / **66091** / **66092** |

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::customer::customer_model::{self, STATUS_ACTIVE};
use crate::business::erp::sales::rebate_dto::{self};
use crate::business::erp::sales::rebate_model;
use crate::business::erp::sales::rebate_model::{RebatePolicyRow, RebateRuleRow};
use crate::business::erp::sales::rebate_settle_dto::{
    self as dto, BaseNumbers, CalcOutcome, PeriodArgs, STATUS_CANCELLED, calc_rebate, normalize_remark, status_label,
};
use crate::business::erp::sales::rebate_settle_model::{
    self as model, ExistingSettleRow, NewSettlement, SettlePageQuery, SettlementRow,
};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0439 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情（两个读接口共用一个码）。
pub const PERM_LIST: &str = "business:erp:rebate-settlement:list";
/// 试算 / 计算落库 / 重算 / 整期跑批（同一件事的四种入口，共用一个动作码）。
pub const PERM_CALC: &str = "business:erp:rebate-settlement:calc";
/// 作废。
pub const PERM_DELETE: &str = "business:erp:rebate-settlement:delete";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const REBATE_SETTLE_PERMISSIONS: [&str; 3] = [PERM_LIST, PERM_CALC, PERM_DELETE];

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`。
fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message.into())
}

/// `409 + 10003`。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// `409 + 66091`：期间无适用政策 / 政策未覆盖整期。
fn no_policy(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_SETTLEMENT_NO_POLICY, message.into())
}

/// `409 + 66092`：同客户同粒度同期间已有未作废结算单。
fn duplicate(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_SETTLEMENT_DUPLICATE, message.into())
}

/// 写路径 DB 错误收口：唯一索引兜底也翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => duplicate(format!(
            "{ctx}：同一客户同一结算期间已有未作废的结算单{}（并发提交时请刷新后重试）",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（期间形状/状态/计价单位配对/金额符号域外）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 参数解析 ====================

/// UUID 入参：非法 → 400（点名是哪个参数）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|error| bad_request(format!("{field} 不是合法 UUID：{error}")))
}

/// 可选 UUID（空白 = 不筛）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 结算状态入参：只认三个域值（其余 400，不静默当「不筛」）。
fn parse_status(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let upper = text.to_ascii_uppercase();
    if !dto::STATUSES.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "status 只认 {}（收到「{text}」）",
            dto::STATUSES.join(" / ")
        )));
    }
    Ok(Some(upper))
}

// ==================== 客户 ====================

/// 客户必须存在、未软删、`ACTIVE`（冻结客户 → 409）。
///
/// 返回客户行（预览/落库都要回显客户编码与名称，不查第二遍）。
async fn require_active_customer(customer_id: Uuid) -> std::result::Result<customer_model::CustomerRow, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| utils::db_error(error, "校验客户存在性"))?
        .ok_or_else(|| not_found(format!("客户 {customer_id} 不存在或已删除，无法结算返利")))?;
    if customer.status != STATUS_ACTIVE {
        return Err(conflict(format!(
            "客户「{}」已冻结（{}），不能结算返利：冻结客户不再产生新返利，\
             解冻后可按同一期间重新计算",
            customer.name, customer.status
        )));
    }
    Ok(customer)
}

// ==================== 政策（闸③：覆盖整期） ====================

/// 取「该客户该粒度、覆盖整个结算期间」的启用政策 + 全部档位。
///
/// 复用 M3-15 的 [`rebate_model::active_policy_of_customer`]（按日 + 启用 + 有效期，
/// 重叠闸保证同客户同粒度当日最多一条），再补一条**覆盖整期**的判定：
/// 政策必须在期间首日与期间末日**两天都生效**（有效期是连续区间，两头都在 ⇒ 整段都覆盖）。
async fn load_policy(
    customer_id: Uuid,
    period: &PeriodArgs,
) -> std::result::Result<(RebatePolicyRow, Vec<RebateRuleRow>), ApiError> {
    let label = rebate_dto::settle_label(&period.settle_period);
    let policy = rebate_model::active_policy_of_customer(customer_id, &period.settle_period, period.from)
        .await
        .map_err(|error| utils::db_error(error, "查询适用返利政策"))?;
    let Some(policy) = policy else {
        // 「首日没有生效政策」有两种原因：压根没建 / 建了但还没生效 —— 报文要说清是哪一种
        let window = model::policy_window(customer_id, &period.settle_period)
            .await
            .map_err(|error| utils::db_error(error, "查询最近一份返利政策"))?;
        return Err(match window {
            None => no_policy(format!(
                "客户还没有任何启用的{label}返利政策：请先在「返利政策」页给该客户建一份 \
                 settle_period={settle_period}、生效日不晚于 {from} 的政策，再回来结算 {period_code}",
                settle_period = period.settle_period,
                from = period.from,
                period_code = period.period_code,
            )),
            Some(window) => no_policy(format!(
                "客户在 {from}（{period_code} 首日）没有生效的{label}返利政策：\
                 最近一份 {policy_no}（{policy_name}）有效期 {window_from} ~ {window_to}，尚未覆盖期间首日。\
                 请把生效日改到 {from} 之前，或把期间改成政策真正覆盖的那一期",
                from = period.from,
                period_code = period.period_code,
                policy_no = window.policy_no,
                policy_name = window.policy_name,
                window_from = window.effective_from,
                window_to = window
                    .effective_to
                    .map(|day| day.to_string())
                    .unwrap_or_else(|| "长期".to_string()),
            )),
        });
    };

    let last_day = period.last_day();
    if !policy.is_active_on(last_day) {
        return Err(no_policy(format!(
            "返利政策 {policy_no}（{policy_name}）没有覆盖整个结算期间：\
             政策有效期 {} ~ {}，本次期间需要覆盖到 {last_day}。\
             半期套一份政策算出来的金额有争议，因此本单不算 —— \
             请把政策失效日改到 {last_day} 之后，或把期间改成政策真正覆盖的那一期",
            policy.effective_from,
            policy
                .effective_to
                .map(|day| day.to_string())
                .unwrap_or_else(|| "长期".to_string()),
            policy_no = policy.policy_no,
            policy_name = policy.policy_name,
        )));
    }

    let rules = rebate_model::rules_of(policy.id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利政策档位"))?;
    if rules.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_REBATE_RULE_INVALID,
            format!(
                "返利政策 {policy_no}（{policy_name}）没有任何档位规则，无法落档：\
                 请在返利政策页补上档位（按量/按额 1 档，阶梯 2~20 档）",
                policy_no = policy.policy_no,
                policy_name = policy.policy_name
            ),
        ));
    }
    Ok((policy, rules))
}

/// 政策行 + 档位行 → 政策全量快照（落 `policy_snapshot` JSONB）。
fn snapshot_of(policy: &RebatePolicyRow, rules: &[RebateRuleRow]) -> dto::PolicySnapshot {
    dto::PolicySnapshot {
        policy_no: policy.policy_no.clone(),
        policy_name: policy.policy_name.clone(),
        rule_type: policy.rule_type.clone(),
        rate_unit: policy.rate_unit.clone(),
        effective_from: policy.effective_from,
        effective_to: policy.effective_to,
        tiers: rules
            .iter()
            .map(|rule| dto::PolicyTierSnapshot::from(&rule.to_write()))
            .collect(),
    }
}

// ==================== 一次完整计算 ====================

/// 一次计算的全部中间结果（试算与落库共用，保证「看到的数」= 「落库的数」）。
struct Computed {
    policy: RebatePolicyRow,
    rules: Vec<RebateRuleRow>,
    base: BaseNumbers,
    outcome: CalcOutcome,
}

/// 按「政策 → 基数 → 落档 → 应返」算出结果（**不写库**）。
async fn compute(customer_id: Uuid, period: &PeriodArgs) -> std::result::Result<Computed, ApiError> {
    let (policy, rules) = load_policy(customer_id, period).await?;
    let base = model::load_base(customer_id, period.from, period.to)
        .await
        .map_err(|error| utils::db_error(error, "汇总期间销售与退货基数"))?;
    let rule_writes = rules.iter().map(RebateRuleRow::to_write).collect::<Vec<_>>();
    let outcome = calc_rebate(&policy.rule_type, &policy.rate_unit, &rule_writes, &base)?;
    Ok(Computed {
        policy,
        rules,
        base,
        outcome,
    })
}

// ==================== 出参装配 ====================

/// 结算单行 → 出参。
fn to_res(row: &SettlementRow) -> dto::RebateSettleRes {
    dto::RebateSettleRes {
        id: row.id.to_string(),
        settlement_no: row.settlement_no.clone(),
        customer_id: row.customer_id.to_string(),
        customer_code: row.customer_code.clone(),
        customer_name: row.customer_name.clone(),
        settle_period: row.settle_period.clone(),
        settle_period_label: rebate_dto::settle_label(&row.settle_period).to_string(),
        period_code: row.period_code.clone(),
        period_from: row.period_from,
        period_to: row.period_to,
        period_last_day: row.period_to.pred_opt().unwrap_or(row.period_to),
        policy_id: row.policy_id.to_string(),
        policy_no: row.policy_no.clone(),
        policy_name: row.policy_name.clone(),
        rule_type: row.rule_type.clone(),
        rule_type_label: rebate_dto::rule_type_label(&row.rule_type).to_string(),
        rate_unit: row.rate_unit.clone(),
        rate_unit_label: rebate_dto::unit_label(&row.rate_unit).to_string(),
        policy_effective_from: row.policy_effective_from,
        policy_effective_to: row.policy_effective_to,
        policy_tiers: row.policy_snapshot.tiers.clone(),
        base: dto::RebateBaseRes::from(&row.base),
        tier_seq: row.tier_seq,
        tier_from: row.tier_from,
        tier_to: row.tier_to,
        rate: row.rate,
        rebate_amount: row.rebate_amount,
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        remark: row.remark.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        invoice_id: row.invoice_id.map(|value| value.to_string()),
        invoice_no: row.invoice_no.clone(),
        settled_at: row.settled_at,
    }
}

/// 已有结算单摘要。
fn to_brief(row: &ExistingSettleRow) -> dto::RebateSettleBriefRes {
    dto::RebateSettleBriefRes {
        id: row.id.to_string(),
        settlement_no: row.settlement_no.clone(),
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        rebate_amount: row.rebate_amount,
    }
}

// ==================== 编排：试算 ====================

/// 试算（**不落库**）：把「基数 → 落档 → 应返」摊开给人看，并回显该期间是否已经算过。
///
/// 没有适用政策时不报错（试算的作用之一就是「看看有没有政策」），
/// 此时 `policy = null`、`rebate_amount = 0`，`warnings` 里写明原因。
pub async fn preview_of(req: dto::RebateSettleReq) -> std::result::Result<dto::RebatePreviewRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let period = PeriodArgs::resolve(&req.settle_period, &req.period_code)?;
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| utils::db_error(error, "校验客户存在性"))?
        .ok_or_else(|| not_found(format!("客户 {customer_id} 不存在或已删除，无法试算返利")))?;
    let existing = model::alive_of_period(db(), customer_id, &period.settle_period, &period.period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询该期间已有结算单"))?;

    let base = model::load_base(customer_id, period.from, period.to)
        .await
        .map_err(|error| utils::db_error(error, "汇总期间销售与退货基数"))?;

    let mut warnings = Vec::new();
    let policy = match load_policy(customer_id, &period).await {
        Ok((policy, rules)) => Some((policy, rules)),
        Err(error) if error.code == error_code::ERP_REBATE_SETTLEMENT_NO_POLICY => {
            warnings.push(error.msg.clone());
            None
        }
        Err(error) => return Err(error),
    };

    let mut res = dto::RebatePreviewRes {
        customer_id: customer_id.to_string(),
        customer_code: Some(customer.code.clone()),
        customer_name: Some(customer.name.clone()),
        settle_period: period.settle_period.clone(),
        settle_period_label: rebate_dto::settle_label(&period.settle_period).to_string(),
        period_code: period.period_code.clone(),
        period_from: period.from,
        period_to: period.to,
        period_last_day: period.last_day(),
        policy: None,
        base: dto::RebateBaseRes::from(&base),
        tier_seq: None,
        tier_from: Decimal::ZERO,
        tier_to: None,
        rate: Decimal::ZERO,
        rebate_amount: Decimal::ZERO,
        warnings: Vec::new(),
        existing: existing.as_ref().map(to_brief),
    };

    if let Some((policy, rules)) = policy {
        let rule_writes = rules.iter().map(RebateRuleRow::to_write).collect::<Vec<_>>();
        let outcome = calc_rebate(&policy.rule_type, &policy.rate_unit, &rule_writes, &base)?;
        res.policy = Some(dto::RebatePreviewPolicyRes {
            policy_id: policy.id.to_string(),
            policy_no: policy.policy_no.clone(),
            policy_name: policy.policy_name.clone(),
            rule_type: policy.rule_type.clone(),
            rate_unit: policy.rate_unit.clone(),
            effective_from: policy.effective_from,
            effective_to: policy.effective_to,
            tiers: rules
                .iter()
                .map(|rule| dto::PolicyTierSnapshot::from(&rule.to_write()))
                .collect(),
        });
        res.tier_seq = Some(outcome.tier.seq);
        res.tier_from = outcome.tier.tier_from;
        res.tier_to = outcome.tier.tier_to;
        res.rate = outcome.tier.rate;
        res.rebate_amount = outcome.rebate_amount;
        warnings.extend(outcome.warnings);
    }

    if let Some(existing) = &res.existing {
        warnings.push(format!(
            "该客户该期间已有结算单 {}（{}）：要改口径请用「重算」（仅已计算状态可重算），\
             要重来请先作废",
            existing.settlement_no,
            status_label(&existing.status)
        ));
    }
    res.warnings = warnings;
    Ok(res)
}

// ==================== 编排：计算落库 / 重算 / 作废 ====================

/// 单客户计算的结果（跑批要区分「新建」与「已有」）。
enum SettleOutcome {
    /// 新建成功。
    Created(Box<dto::RebateSettleRes>),
    /// 该期间已有未作废结算单（跑批时跳过，不报错）。
    Exists(ExistingSettleRow),
}

/// 单客户「计算并落库」：**客户级 advisory lock → 查重 → 计算 → 取号 → 插单**，一个事务。
///
/// 必须取 `rebate_model::lock_customer`（与 M3-15 建政策同一把锁）：两个并发请求同时
/// 结算同一客户时，不加锁会双双通过「查重」，然后靠唯一索引把其中一个打成 500。
async fn settle_inner(
    operator: Option<Uuid>,
    customer_id: Uuid,
    period: &PeriodArgs,
    remark: Option<&str>,
    allow_exists: bool,
) -> std::result::Result<SettleOutcome, ApiError> {
    let computed = compute(customer_id, period).await?;
    let snapshot = snapshot_of(&computed.policy, &computed.rules);
    let snapshot_text =
        serde_json::to_string(&snapshot).map_err(|error| bad_request(format!("政策快照序列化失败：{error}")))?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启返利结算事务"))?;
    if let Err(error) = rebate_model::lock_customer(&tx, customer_id).await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "锁定客户（返利结算串行化）"));
    }
    let existing = match model::alive_of_period(&tx, customer_id, &period.settle_period, &period.period_code).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询该期间已有结算单"));
        }
    };
    if let Some(existing) = existing {
        let _ = tx.rollback().await;
        if allow_exists {
            return Ok(SettleOutcome::Exists(existing));
        }
        return Err(duplicate(format!(
            "客户该期间已有结算单 {}（{}，应返 {}）：\
             要改口径请用「重算」（仅已计算状态可重算）；要重来请先作废；\
             已转应付/红字发票的不能再算 —— 那是红冲/调账",
            existing.settlement_no,
            status_label(&existing.status),
            existing.rebate_amount
        )));
    }

    let new = NewSettlement {
        settlement_no: "", // 取号后回填（见下）
        customer_id,
        settle_period: &period.settle_period,
        period_code: &period.period_code,
        period_from: period.from,
        period_to: period.to,
        policy_id: computed.policy.id,
        policy_no: &computed.policy.policy_no,
        policy_name: &computed.policy.policy_name,
        rule_type: &computed.policy.rule_type,
        rate_unit: &computed.policy.rate_unit,
        policy_effective_from: computed.policy.effective_from,
        policy_effective_to: computed.policy.effective_to,
        policy_snapshot: snapshot_text,
        base: computed.base.clone(),
        tier_seq: computed.outcome.tier.seq,
        tier_from: computed.outcome.tier.tier_from,
        tier_to: computed.outcome.tier.tier_to,
        rate: computed.outcome.tier.rate,
        rebate_amount: computed.outcome.rebate_amount,
        remark,
    };
    // 取号 → 回填单号（同一个事务：推了号却没落库会让号永久占用）
    let settlement_no = match model::next_settlement_no(&tx, Utc::now()).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "取返利结算单号"));
        }
    };
    let new = NewSettlement {
        settlement_no: &settlement_no,
        ..new
    };
    let created = model::insert_settlement(&tx, &new, operator, Utc::now()).await;
    let id = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建返利结算单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交返利结算事务"))?;
    let res = detail_of(id).await?;
    debug_assert_eq!(res.settlement_no, settlement_no, "回读单号必须与取号结果一致");
    Ok(SettleOutcome::Created(Box::new(res)))
}

/// 计算并落库（单客户）。
pub async fn settle_of(
    operator: Option<Uuid>,
    req: dto::RebateSettleReq,
) -> std::result::Result<dto::RebateSettleRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let period = PeriodArgs::resolve(&req.settle_period, &req.period_code)?;
    require_active_customer(customer_id).await?;
    let remark = normalize_remark(req.remark.as_deref())?;
    match settle_inner(operator, customer_id, &period, remark.as_deref(), false).await? {
        SettleOutcome::Created(res) => Ok(*res),
        // allow_exists = false 时不可能走到这里（上面已经 Err），保守兜底成 409
        SettleOutcome::Exists(existing) => Err(duplicate(format!(
            "客户该期间已有结算单 {}（{}）",
            existing.settlement_no,
            status_label(&existing.status)
        ))),
    }
}

/// 重算（仅 `DRAFT`）：政策/订单/退货变了之后刷新基数与金额，**政策快照一并刷新**。
///
/// 为什么重算要刷新政策快照：重算的语义是「按现在的口径重算这一期」，快照必须与算出来的
/// 金额一致 —— 留着旧快照会得到「金额按新政策、说明按旧政策」的分裂单据。
/// `SETTLED` 的不能重算（钱已经转成应付/红字发票了，改数就是账实不符）。
pub async fn recalc_of(
    operator: Option<Uuid>,
    id_raw: &str,
    remark: Option<&str>,
) -> std::result::Result<dto::RebateSettleRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let remark = normalize_remark(remark)?;
    let current = model::find_settlement(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待重算返利结算单"))?
        .ok_or_else(|| not_found("返利结算单不存在或已删除"))?;
    if current.status != dto::STATUS_DRAFT {
        return Err(conflict(format!(
            "返利结算单 {} 当前是「{}」，不能重算：已转应付/红字发票的金额只能走红冲/调账；\
             已作废的要重来请新建一张",
            current.settlement_no,
            status_label(&current.status)
        )));
    }
    // 期间与客户来自单据本身（重算不接受改期间：改了期间就是另一张单）
    let period = PeriodArgs {
        settle_period: current.settle_period.clone(),
        period_code: current.period_code.clone(),
        from: current.period_from,
        to: current.period_to,
    };
    require_active_customer(current.customer_id).await?;
    let computed = compute(current.customer_id, &period).await?;
    let snapshot_text = serde_json::to_string(&snapshot_of(&computed.policy, &computed.rules))
        .map_err(|error| bad_request(format!("政策快照序列化失败：{error}")))?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启返利重算事务"))?;
    let locked = match model::lock_settlement(&tx, id).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定返利结算单"));
        }
    };
    if !locked {
        let _ = tx.rollback().await;
        return Err(not_found("返利结算单不存在或已被删除"));
    }
    if let Err(error) = rebate_model::lock_customer(&tx, current.customer_id).await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "锁定客户（返利结算串行化）"));
    }
    let new = NewSettlement {
        settlement_no: &current.settlement_no,
        customer_id: current.customer_id,
        settle_period: &current.settle_period,
        period_code: &current.period_code,
        period_from: current.period_from,
        period_to: current.period_to,
        policy_id: computed.policy.id,
        policy_no: &computed.policy.policy_no,
        policy_name: &computed.policy.policy_name,
        rule_type: &computed.policy.rule_type,
        rate_unit: &computed.policy.rate_unit,
        policy_effective_from: computed.policy.effective_from,
        policy_effective_to: computed.policy.effective_to,
        policy_snapshot: snapshot_text,
        base: computed.base.clone(),
        tier_seq: computed.outcome.tier.seq,
        tier_from: computed.outcome.tier.tier_from,
        tier_to: computed.outcome.tier.tier_to,
        rate: computed.outcome.tier.rate,
        rebate_amount: computed.outcome.rebate_amount,
        remark: remark.as_deref().or(current.remark.as_deref()),
    };
    match model::update_calc(&tx, id, &new, operator, Utc::now()).await {
        Ok(affected) if affected > 0 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Err(conflict(
                "返利结算单在保存期间被作废或转应付，请刷新后重试（已转应付的不能重算）",
            ));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "重算返利结算单"));
        }
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交返利重算事务"))?;
    detail_of(id).await
}

/// 作废（仅 `DRAFT`）：行保留供审计，同期间可重开一张。
pub async fn cancel_of(
    operator: Option<Uuid>,
    id_raw: &str,
    remark: Option<&str>,
) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let remark = normalize_remark(remark)?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启返利作废事务"))?;
    let locked = match model::lock_settlement(&tx, id).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定返利结算单"));
        }
    };
    if !locked {
        let _ = tx.rollback().await;
        return Err(not_found("返利结算单不存在或已删除"));
    }
    match model::mark_cancelled(&tx, id, operator, remark.as_deref(), Utc::now()).await {
        Ok(affected) if affected > 0 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            // 锁到行但状态不是 DRAFT ⇒ 已转应付或已作废
            let current = model::find_settlement(id).await.ok().flatten();
            let hint = current
                .map(|row| format!("（当前：{} / {}）", row.settlement_no, status_label(&row.status)))
                .unwrap_or_default();
            return Err(conflict(format!(
                "只有「已计算」状态的返利结算单能作废{hint}：\
                 已转应付/红字发票的不能作废 —— 钱已经动了，要走红冲/调账"
            )));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "作废返利结算单"));
        }
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交返利作废事务"))?;
    if model::find_settlement(id)
        .await
        .map_err(|error| utils::db_error(error, "回读作废后的返利结算单"))?
        .is_none_or(|row| row.status != STATUS_CANCELLED)
    {
        return Err(conflict("作废后回读状态不是 CANCELLED，请刷新确认"));
    }
    Ok(())
}

// ==================== 编排：整期跑批 ====================

/// 整期跑批：对该期间**有覆盖整期政策**的每个客户各算一张（逐客户一个事务）。
///
/// 幂等：已有未作废结算单的客户返回 `EXISTS`（不重复算、不报错），可反复跑。
pub async fn run_of(
    operator: Option<Uuid>,
    req: dto::RebateRunReq,
) -> std::result::Result<dto::RebateRunRes, ApiError> {
    let period = PeriodArgs::resolve(&req.settle_period, &req.period_code)?;
    let remark = normalize_remark(req.remark.as_deref())?;
    let wanted = req
        .customer_ids
        .iter()
        .map(|raw| parse_uuid(raw, "customer_ids"))
        .collect::<std::result::Result<Vec<_>, ApiError>>()?;
    let policies = model::policies_covering_period(&period.settle_period, period.from, period.last_day(), &wanted)
        .await
        .map_err(|error| utils::db_error(error, "查询覆盖该期间的返利政策"))?;

    let mut items = Vec::with_capacity(policies.len());
    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for row in policies.iter() {
        // 同一客户理论上只有一份覆盖整期的政策（重叠闸保证）；防御性去重，避免同一客户算两次
        if !seen.insert(row.customer_id) {
            continue;
        }
        let item = match settle_inner(operator, row.customer_id, &period, remark.as_deref(), true).await {
            Ok(SettleOutcome::Created(res)) => {
                created += 1;
                dto::RebateRunItemRes {
                    customer_id: row.customer_id.to_string(),
                    customer_code: res.customer_code.clone().or_else(|| row.customer_code.clone()),
                    customer_name: res.customer_name.clone().or_else(|| row.customer_name.clone()),
                    outcome: "CREATED".to_string(),
                    settlement_id: Some(res.id.clone()),
                    settlement_no: Some(res.settlement_no.clone()),
                    status: Some(res.status.clone()),
                    rebate_amount: Some(res.rebate_amount),
                    message: None,
                }
            }
            Ok(SettleOutcome::Exists(existing)) => {
                skipped += 1;
                dto::RebateRunItemRes {
                    customer_id: row.customer_id.to_string(),
                    customer_code: row.customer_code.clone(),
                    customer_name: row.customer_name.clone(),
                    outcome: "EXISTS".to_string(),
                    settlement_id: Some(existing.id.to_string()),
                    settlement_no: Some(existing.settlement_no),
                    status: Some(existing.status),
                    rebate_amount: Some(existing.rebate_amount),
                    message: Some("该期间已有结算单，跳过（要改口径请用重算）".to_string()),
                }
            }
            Err(error) => {
                failed += 1;
                dto::RebateRunItemRes {
                    customer_id: row.customer_id.to_string(),
                    customer_code: row.customer_code.clone(),
                    customer_name: row.customer_name.clone(),
                    outcome: "FAILED".to_string(),
                    settlement_id: None,
                    settlement_no: None,
                    status: None,
                    rebate_amount: None,
                    message: Some(error.msg),
                }
            }
        };
        items.push(item);
    }

    Ok(dto::RebateRunRes {
        settle_period: period.settle_period.clone(),
        settle_period_label: rebate_dto::settle_label(&period.settle_period).to_string(),
        period_code: period.period_code.clone(),
        period_from: period.from,
        period_to: period.to,
        total: items.len(),
        created,
        skipped,
        failed,
        items,
    })
}

// ==================== 编排：列表 / 详情 ====================

/// 分页列出返利结算单。
pub async fn page_settlements_of(
    filter: dto::RebateSettleFilter,
) -> std::result::Result<PageData<Vec<dto::RebateSettleRes>>, ApiError> {
    let customer_id = parse_optional_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let policy_id = parse_optional_uuid(filter.policy_id.as_deref(), "policy_id")?;
    let settle_period = match filter
        .settle_period
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(rebate_dto::checked_settle_period(raw)?),
    };
    let period_code = match filter
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(sales_target_period_code(raw)?),
    };
    let status = parse_status(filter.status.as_deref())?;
    let query = SettlePageQuery {
        keyword: filter.keyword.as_deref(),
        customer_id,
        settle_period: settle_period.as_deref(),
        period_code: period_code.as_deref(),
        status: status.as_deref(),
        policy_id,
        page: filter.page,
        page_size: filter.page_size,
    };
    let (rows, total) = model::settle_page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询返利结算单"))?;
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(to_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 期间码筛选归一化（只校验形状能算出日期，不要求与粒度互配 —— 列表筛选允许只按期间码筛）。
fn sales_target_period_code(raw: &str) -> std::result::Result<String, ApiError> {
    crate::business::erp::sales::sales_target_dto::Period::validate_code(raw)
}

/// 结算单详情（含政策全量档位快照）。不存在 → 404。
pub async fn detail_settlement_of(id_raw: &str) -> std::result::Result<dto::RebateSettleRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 内部详情（创建/重算成功后走同一条回读路径）。
async fn detail_of(id: Uuid) -> std::result::Result<dto::RebateSettleRes, ApiError> {
    let row = model::find_settlement(id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利结算单"))?
        .ok_or_else(|| not_found("返利结算单不存在或已删除"))?;
    Ok(to_res(&row))
}

// ==================== HTTP 入口 ====================

/// 返利试算（不落库）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/preview",
    method = "POST",
    permission = "business:erp:rebate-settlement:calc"
)]
#[nik::post("/business/erp/rebate-settlement/preview")]
pub async fn rebate_settle_preview_api(Json(req): Json<dto::RebateSettleReq>) -> ResResult<dto::RebatePreviewRes> {
    Ok(Res::ok(preview_of(req).await?))
}

/// 返利计算并落库（单客户）。
#[nik::permission(
    path = "/business/erp/rebate-settlement",
    method = "POST",
    permission = "business:erp:rebate-settlement:calc"
)]
#[nik::post("/business/erp/rebate-settlement")]
pub async fn rebate_settle_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::RebateSettleReq>,
) -> ResResult<dto::RebateSettleRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(settle_of(operator, req).await?))
}

/// 返利整期跑批（逐客户一个事务；已有则跳过）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/run",
    method = "POST",
    permission = "business:erp:rebate-settlement:calc"
)]
#[nik::post("/business/erp/rebate-settlement/run")]
pub async fn rebate_settle_run_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::RebateRunReq>,
) -> ResResult<dto::RebateRunRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(run_of(operator, req).await?))
}

/// 返利结算重算（仅 `DRAFT`）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/{id}/recalc",
    method = "POST",
    permission = "business:erp:rebate-settlement:calc"
)]
#[nik::post("/business/erp/rebate-settlement/{id}/recalc")]
pub async fn rebate_settle_recalc_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::RebateSettleReq>,
) -> ResResult<dto::RebateSettleRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(recalc_of(operator, &id, req.remark.as_deref()).await?))
}

/// 返利结算分页。
#[nik::permission(
    path = "/business/erp/rebate-settlement/page",
    method = "GET",
    permission = "business:erp:rebate-settlement:list"
)]
#[nik::get("/business/erp/rebate-settlement/page")]
pub async fn rebate_settle_page_api(
    Query(filter): Query<dto::RebateSettleFilter>,
) -> ResResult<PageData<Vec<dto::RebateSettleRes>>> {
    Ok(Res::ok(page_settlements_of(filter).await?))
}

/// 返利结算详情。
#[nik::permission(
    path = "/business/erp/rebate-settlement/{id}",
    method = "GET",
    permission = "business:erp:rebate-settlement:list"
)]
#[nik::get("/business/erp/rebate-settlement/{id}")]
pub async fn rebate_settle_detail_api(Path(id): Path<String>) -> ResResult<dto::RebateSettleRes> {
    Ok(Res::ok(detail_settlement_of(&id).await?))
}

/// 作废返利结算（仅 `DRAFT`；行保留供审计）。
#[nik::permission(
    path = "/business/erp/rebate-settlement/{id}",
    method = "DELETE",
    permission = "business:erp:rebate-settlement:delete"
)]
#[nik::delete("/business/erp/rebate-settlement/{id}")]
pub async fn rebate_settle_cancel_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(req): Query<dto::RebateSettleReq>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    cancel_of(operator, &id, req.remark.as_deref()).await?;
    Ok(Res::ok(()))
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码与 V0439 逐字一致；三个码互不相同；写码不能是读码。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(REBATE_SETTLE_PERMISSIONS, [PERM_LIST, PERM_CALC, PERM_DELETE]);
        assert_eq!(PERM_LIST, "business:erp:rebate-settlement:list");
        assert_eq!(PERM_CALC, "business:erp:rebate-settlement:calc");
        assert_eq!(PERM_DELETE, "business:erp:rebate-settlement:delete");
        let unique: std::collections::BTreeSet<&str> = REBATE_SETTLE_PERMISSIONS.into_iter().collect();
        assert_eq!(unique.len(), 3, "三个码必须互不相同");
        for code in [PERM_CALC, PERM_DELETE] {
            assert!(!code.ends_with(":list"), "{code} 是写接口的码，不能是 :list");
        }
        println!("[M3-16] 权限码 list/calc/delete（三码互不相同、写码非读码）✓");
    }

    /// 错误码绑定：66091 无政策 / 66092 期间重复。
    #[test]
    fn error_codes_are_bound() {
        assert_eq!(no_policy("x").code, 66091);
        assert_eq!(no_policy("x").status, 409);
        assert_eq!(duplicate("x").code, 66092);
        assert_eq!(duplicate("x").status, 409);
        assert_ne!(no_policy("x").code, duplicate("x").code, "两道闸必须能分辨");
        println!("[M3-16] 66091 无政策 / 66092 期间重复（均 409，码不混用）✓");
    }

    /// 非法 UUID / 空 id 必须 400 + 10001，且文案点名字段。
    #[test]
    fn invalid_uuid_is_400_and_names_the_field() {
        let error = parse_uuid("not-a-uuid", "customer_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("customer_id"), "{}", error.msg);
        assert!(
            parse_optional_uuid(Some("  "), "customer_id")
                .expect("空白不报错")
                .is_none()
        );
        println!("[M3-16] 非法/空 UUID → 400 + 10001 并点名字段 ✓");
    }

    /// 状态筛选只认三个域值（拼错的 `DRFAT` 不许静默当「不筛」）。
    #[test]
    fn status_filter_is_strict() {
        assert_eq!(
            parse_status(Some(" draft ")).expect("小写归一"),
            Some("DRAFT".to_string())
        );
        assert_eq!(parse_status(Some("")).expect("空串不筛"), None);
        for bad in ["DRFAT", "PAID", "1"] {
            let error = parse_status(Some(bad)).expect_err("域外状态必须被拒");
            assert_eq!(error.status, 400);
            assert!(error.msg.contains("DRAFT"), "{}", error.msg);
        }
        println!("[M3-16] 状态筛选只认 DRAFT/SETTLED/CANCELLED（域外 400）✓");
    }

    /// 期间码筛选：形状非法 400；合法则归一化（`2026-q1` → `2026-Q1`）。
    #[test]
    fn period_code_filter_normalises() {
        assert_eq!(sales_target_period_code("2026-10").expect("月度"), "2026-10");
        assert_eq!(sales_target_period_code(" 2026-q1 ").expect("季度"), "2026-Q1");
        assert!(sales_target_period_code("2026/10").is_err());
        println!("[M3-16] 期间码筛选归一化（月度/季度；非法形状 400）✓");
    }
}
