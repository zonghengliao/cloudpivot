//! 佣金计提 + 结算单（销售链 · SO-36 / ARCH-7 §15.4.3）。核心口径：**按已收款计佣，不按开单计佣**。
//!
//! 三张表见 `migrations/business/V0309__sales_commission.up.sql`，
//! 本卡补的库层改动见 `V0311__commission_settle_setup.up.sql`。
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | POST | `/business/erp/commission/settle` | `business:erp:commission-settle:create` |
//! | GET | `/business/erp/commission/page` | `business:erp:commission-settle:list` |
//! | GET | `/business/erp/commission/{id}` | `business:erp:commission-settle:list` |
//! | PATCH | `/business/erp/commission/{id}/status` | `business:erp:commission-settle:status` |
//!
//! # 计提算法（确定性，全部可单测）
//!
//! ```text
//! ① 候选应收 = 该销售员负责的订单 → 发货 → 销售出库 → 应收（source_type='SALES_OUT'）
//! ② 本期基数 delta = 应收.received_amount（已核销 / 真收到钱） − 已认领基数
//!    · 已认领 = 该应收在**存活**结算单里的 base_amount 之和（草稿也算认领）
//!    · delta > 0 计提；delta < 0 = 退货 / 收款退款把已收打回去了 → 冲回；= 0 跳过
//! ③ 一张订单可能有多张应收（分批发货）、订单行可能跨物料：
//!    按订单行的未税金额把 delta 摊到各物料桶（末桶吸收舍入残差，桶之和恒等于 delta）
//! ④ 每个桶按 **销售员 > 物料 > 全局** 选规则（同范围多条取生效日新、再取创建新、再取 id 小）
//! ⑤ 佣金 = PERCENT: 基数×比率/100；FIXED: 固定额（基数 < min_amount 不计；封顶 max_amount）
//!    金额四舍五入到分（2 位）；退货冲回行取负、封顶对称
//! ⑥ 汇总写 biz_commission_settlement（total_sales 负值按 0 兜底），明细写 biz_commission_item
//! ```
//!
//! # 凭证（复用 `voucher::post_voucher`，不手搓借贷平衡）
//!
//! | 时点 | 分录 | 说明 |
//! |------|------|------|
//! | DRAFT → APPROVED | 借 5601 销售费用-佣金 / 贷 2241.02 其他应付款-佣金 | 计提入账；佣金为负（纯冲回）时方向相反 |
//! | APPROVED → PAID | 借 2241.02 / 贷 1002 银行存款 | 实际发放，把应付佣金冲平 |
//!
//! ## 科目口径（**与卡面不同，这里有据**）
//!
//! * 卡面写「借 6601 销售费用-佣金」：本仓科目集是**小企业会计制度核心科目**
//!   （V0183 种子），`66xx` 一条都没有；销售费用 = **5601**。V0226 / V0229 遇到同一问题
//!   都是按 5601 / 5602 落地并在迁移头注释里记账，本卡沿用（新种 6601 会让销售费用在
//!   利润表与预算科目树里凭空消失）。
//! * 卡面写「贷 2241」：`2241` 自 V0226 起是**非末级（仅汇总）**科目，
//!   `voucher::post_voucher` 拒绝非末级科目（`non_leaf_codes` 硬闸）。
//!   故 V0311 种末级子科目 **2241.02 其他应付款-佣金**，与 V0226 种 2241.01 同口径。
//!
//! # 为什么状态流转不写 `biz_status_log`
//!
//! 状态日志的 `entity_type` 取值域 = `DocKind`（P2-5 词表收敛，`doc_kind_tests` 把
//! 「Rust 枚举 ↔ 前端 `doc-kinds.ts` 镜像 ↔ 巡检登记表 `10_checks.sql`」三方逐字钉死）。
//! 新增一个 `DocKind::CommissionSettlement` 需要同步改前端镜像与巡检登记 SQL，
//! 属词表收敛的独立改动；本卡用「handler 内 `LEGAL_EDGES` + `WHERE status = 原状态`
//! 乐观锁 + 库层 `ck_biz_cmset_status`」三处兜住状态域与并发，审计凭据是
//! `updated_at` / `update_by` / 两张凭证的 `source_no`（= 结算单号）。
//! **遗留**：接 `biz_status_log` 需与 DocKind 词表扩展同批做（已记进卡内汇报的 residual）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPED: biz_commission_settlement.salesperson_id = 归属列（V0311 起登记进
//!         scope::SCOPED_TABLES），分页走 scope::paginate → 销售员只看得见自己的结算单；
//!         部门类范围因表上没有 dept_id 会安全侧降级为「仅本人」。
//! SCOPE-EXEMPT: biz_commission_rule / biz_commission_item 是配置与明细表（随单可见），
//!         列表入口只有结算单分页；计提批跑是服务端内部动作（权限码闸住）。
//! SCOPE-EXEMPT: biz_receivable / biz_sales_order / biz_shipment / biz_stock_out /
//!         biz_voucher / biz_seq 是台账与全局字典，无归属列（见各模块文档）。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::commission_dto::{
    self, CommissionFilter, CommissionItemRes, CommissionPageQuery, CommissionStatusReq,
    STATUS_APPROVED, STATUS_PAID, STATUSES, SettleReq, SettleRes, SettlementDetailRes,
    SettlementRes, SkippedRes,
};
use crate::business::erp::commission_model::{self as model, NewItem, RuleRow};
use crate::business::erp::model::normalize_page;
use crate::business::erp::period;
use crate::business::erp::voucher::{self, EntryLine, PostVoucher};
use crate::business::shared::principal::ScopedPrincipal;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0310 的 sys_menu.auth_code 逐字一致） ====================

/// 生成结算单（计提）。
pub const PERM_CREATE: &str = "business:erp:commission-settle:create";
/// 查询（列表 / 详情）。
pub const PERM_LIST: &str = "business:erp:commission-settle:list";
/// 审批 / 发放。
pub const PERM_STATUS: &str = "business:erp:commission-settle:status";

/// 佣金结算域全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const COMMISSION_PERMISSIONS: [&str; 3] = [PERM_CREATE, PERM_LIST, PERM_STATUS];

// ==================== 规则域值 ====================

/// 规则范围：按销售员。
pub const SCOPE_SALESPERSON: &str = "SALESPERSON";
/// 规则范围：按物料类别。
///
/// 基础版**没有**物料类别表（`biz_material_category` 未建，见 `erp/handler.rs` 的
/// 权限清单注释），故本期 `scope_id` 存 `biz_material.id` —— 语义是「按物料」的
/// 类别粒度近似。扩展版接入分类表后由数据迁移把 scope_id 换成类别 id，
/// 匹配逻辑不变（仍按 `material_id` 命中）。
pub const SCOPE_MATERIAL: &str = "MATERIAL_CATEGORY";
/// 规则范围：全局。
pub const SCOPE_ALL: &str = "ALL";

/// 计算方式：百分比。
pub const CALC_PERCENT: &str = "PERCENT";
/// 计算方式：固定额。
pub const CALC_FIXED: &str = "FIXED";

/// 金额精度：佣金按分（2 位）落库，凭证金额与明细合计因此逐分相等。
pub const MONEY_DP: u32 = 2;
/// 基数精度（与库层 `NUMERIC(18,4)` 一致）。
pub const BASE_DP: u32 = 4;

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message.into())
}

// ==================== 纯逻辑（全部可单测） ====================

/// 规则范围优先级：销售员 0 > 物料 1 > 全局 2；不适用于该销售员 / 物料的规则返回 `None`。
fn scope_rank(rule: &RuleRow, salesperson_id: Uuid, material_id: Option<Uuid>) -> Option<u8> {
    match rule.scope_type.as_str() {
        SCOPE_SALESPERSON => (rule.scope_id == Some(salesperson_id)).then_some(0),
        SCOPE_MATERIAL => match (rule.scope_id, material_id) {
            (Some(scope), Some(material)) if scope == material => Some(1),
            _ => None,
        },
        SCOPE_ALL => Some(2),
        // 域外值不该出现（库层 CHECK 挡着），但也不许静默命中
        _ => None,
    }
}

/// 规则在指定日期是否生效（`effective_to` 是**含**的，与 V0309 注释一致）。
pub fn rule_is_effective_on(rule: &RuleRow, date: NaiveDate) -> bool {
    rule.effective_from <= date && rule.effective_to.is_none_or(|to| date <= to)
}

/// 选规则：**销售员 > 物料 > 全局**；同范围多条时取生效日最新，再取创建最新，最后 id 小。
///
/// 「同范围多条」是真实存在的（换季调比率：旧规则 effective_to 写月底、新规则次月生效，
/// 月中重算时两条都可能覆盖同一天），所以必须有全序，否则同一单两次计提可能算出两个数。
pub fn pick_rule(
    rules: &[RuleRow],
    salesperson_id: Uuid,
    material_id: Option<Uuid>,
    date: NaiveDate,
) -> Option<&RuleRow> {
    rules
        .iter()
        .filter(|rule| rule_is_effective_on(rule, date))
        .filter_map(|rule| scope_rank(rule, salesperson_id, material_id).map(|rank| (rank, rule)))
        .min_by(|(rank_a, a), (rank_b, b)| {
            rank_a
                .cmp(rank_b)
                .then_with(|| b.effective_from.cmp(&a.effective_from))
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.id.cmp(&b.id))
        })
        .map(|(_, rule)| rule)
}

/// 单行佣金：返回 `(比率快照, 佣金)`；`None` = 这一行不计佣（未达起算额 / 结果为 0）。
///
/// * `min_amount`：佣金基数的**绝对值**低于起算额则不计佣（正负对称）；
/// * `max_amount`：单行佣金按绝对值封顶（冲回行同样受限，否则冲多了就是反向发钱）；
/// * `FIXED`：与基数无关的固定额（一张应收的一份佣金），冲回时取负。
pub fn commission_of(rule: &RuleRow, base: Decimal) -> Option<(Decimal, Decimal)> {
    let magnitude = base.abs();
    if magnitude <= Decimal::ZERO || magnitude < rule.min_amount {
        return None;
    }
    let raw = match rule.calc_type.as_str() {
        CALC_PERCENT => {
            if rule.rate <= Decimal::ZERO {
                return None;
            }
            base * rule.rate / Decimal::from(100)
        }
        CALC_FIXED => {
            if rule.rate <= Decimal::ZERO {
                return None;
            }
            if base > Decimal::ZERO {
                rule.rate
            } else {
                -rule.rate
            }
        }
        _ => return None,
    };
    let mut amount = raw.round_dp(MONEY_DP);
    if let Some(cap) = rule.max_amount
        && cap > Decimal::ZERO
        && amount.abs() > cap
    {
        amount = if amount < Decimal::ZERO { -cap } else { cap };
        amount = amount.round_dp(MONEY_DP);
    }
    if amount == Decimal::ZERO {
        return None;
    }
    Some((rule.rate, amount))
}

/// 把一笔基数按权重摊到多个桶：**末桶吸收全部舍入残差**，桶之和恒等于 `base`。
///
/// 这是「已认领基数」可逐期复算的前提：任何一期摊出来加起来都必须正好等于
/// 该期从应收上取走的 delta，否则下期会把几厘钱反复计提。
pub fn allocate(base: Decimal, weights: &[Decimal]) -> Vec<Decimal> {
    if weights.is_empty() {
        return vec![base];
    }
    let total: Decimal = weights.iter().copied().sum();
    let usable: Vec<Decimal> = if total > Decimal::ZERO {
        weights.to_vec()
    } else {
        // 订单行金额全是 0（赠品 / 历史脏数据）→ 等额分摊，同样末桶吸收残差
        vec![Decimal::ONE; weights.len()]
    };
    let divisor: Decimal = usable.iter().copied().sum();
    let mut out = Vec::with_capacity(usable.len());
    let mut assigned = Decimal::ZERO;
    for (index, weight) in usable.iter().enumerate() {
        if index + 1 == usable.len() {
            out.push((base - assigned).round_dp(BASE_DP));
        } else {
            let part = (base * *weight / divisor).round_dp(BASE_DP);
            assigned += part;
            out.push(part);
        }
    }
    out
}

/// 一个「物料桶」的计提计划（一条明细的来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLine {
    /// 应收 id。
    pub receivable_id: Uuid,
    /// 订单 id。
    pub so_id: Uuid,
    /// 命中的物料（桶键；`None` = 订单无明细，只能命中销售员 / 全局规则）。
    pub material_id: Option<Uuid>,
    /// 基数（有符号）。
    pub base_amount: Decimal,
    /// 规则 id。
    pub rule_id: Uuid,
    /// 比率快照。
    pub rate: Decimal,
    /// 佣金。
    pub commission: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

/// 按物料把订单行汇总成权重（确定性：按 material_id 排序，`None` 在前）。
pub fn weights_by_material(lines: &[model::OrderLineRow]) -> Vec<(Option<Uuid>, Decimal)> {
    let mut buckets: Vec<(Option<Uuid>, Decimal)> = Vec::new();
    for line in lines {
        let weight = if line.amount > Decimal::ZERO {
            line.amount
        } else {
            line.qty
        };
        match buckets
            .iter_mut()
            .find(|(material, _)| *material == line.material_id)
        {
            Some((_, sum)) => *sum += weight,
            None => buckets.push((line.material_id, weight)),
        }
    }
    buckets.sort_by(|(a, _), (b, _)| a.cmp(b));
    buckets
}

/// 单张应收的计提计划（纯函数：给 delta + 订单行 + 规则，出明细行）。
///
/// `delta == 0` 或没命中任何规则 → 空计划（**不认领基数**：基数留给以后有规则的期间）。
pub fn plan_receivable(
    receivable_id: Uuid,
    so_id: Uuid,
    delta: Decimal,
    order_lines: &[model::OrderLineRow],
    rules: &[RuleRow],
    salesperson_id: Uuid,
    date: NaiveDate,
) -> Vec<PlanLine> {
    if delta == Decimal::ZERO {
        return Vec::new();
    }
    let mut buckets = weights_by_material(order_lines);
    if buckets.is_empty() {
        // 订单没有明细（历史脏数据 / 纯服务类订单）：整笔基数走「无物料」桶，
        // 只能命中销售员 / 全局规则 —— 不能因为拆不出物料就把佣金丢掉。
        buckets.push((None, Decimal::ONE));
    }
    let weights: Vec<Decimal> = buckets.iter().map(|(_, weight)| *weight).collect();
    let shares = allocate(delta, &weights);
    let mut plan = Vec::new();
    for ((material_id, _), share) in buckets.iter().zip(shares.iter()) {
        if *share == Decimal::ZERO {
            continue;
        }
        let Some(rule) = pick_rule(rules, salesperson_id, *material_id, date) else {
            continue;
        };
        let Some((rate, commission)) = commission_of(rule, *share) else {
            continue;
        };
        plan.push(PlanLine {
            receivable_id,
            so_id,
            material_id: *material_id,
            base_amount: *share,
            rule_id: rule.id,
            rate,
            commission,
            remark: if *share < Decimal::ZERO {
                Some(format!("退货 / 退款冲回（{}）", rule.rule_name))
            } else {
                None
            },
        });
    }
    plan
}

// ==================== 生成结算单 ====================

/// 生成结算单：`period_code` + （可选）`salesperson_id`。
///
/// 不给销售员时按「有销售员归属的订单」逐个跑一遍 —— 月结就是这么用的：
/// 一次把当月所有已收款都计提掉，没数据的销售员进 `skipped`（不是错误）。
pub async fn settle(
    operator: Option<Uuid>,
    req: SettleReq,
) -> std::result::Result<SettleRes, ApiError> {
    let now = Utc::now();
    let period_code = match req.period_code.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => commission_dto::validate_period_code(raw)?,
        _ => commission_dto::current_period_code(now),
    };
    let remark = req
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);

    let targets = match req.salesperson_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => {
            let id = commission_dto::parse_uuid(raw, "salesperson_id")?;
            vec![(id, String::new())]
        }
        _ => model::salespeople_with_orders(db())
            .await
            .map_err(|error| utils::db_error(error, "枚举有销售员归属的订单"))?,
    };

    let mut settlements = Vec::new();
    let mut skipped = Vec::new();
    for (salesperson_id, name) in targets {
        match settle_one(
            operator,
            salesperson_id,
            &period_code,
            remark.as_deref(),
            now,
        )
        .await?
        {
            Ok(res) => settlements.push(res),
            Err(reason) => skipped.push(SkippedRes {
                salesperson_id: salesperson_id.to_string(),
                salesperson_name: name,
                reason,
            }),
        }
    }
    Ok(SettleRes {
        period_code,
        settlements,
        skipped,
    })
}

/// 单个销售员的计提（**自己开事务**：结算单头 + 明细 + 软删旧草稿同生共死）。
///
/// `Ok(Err(reason))` = 跳过（没有可计佣的已收款，或已有已审批结算单）。
#[allow(clippy::type_complexity)]
async fn settle_one(
    operator: Option<Uuid>,
    salesperson_id: Uuid,
    period_code: &str,
    remark: Option<&str>,
    now: DateTime<Utc>,
) -> std::result::Result<std::result::Result<SettlementRes, String>, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启佣金计提事务"))?;
    let result = async {
        period::ensure_period_writable(&tx, period_code, "佣金计提").await?;
        // 同人同期间只能有一张存活结算单（uk_biz_cmset_period）：已入账的拒绝重算，
        // 草稿先软删再重算（软删即释放它认领的基数）。
        if let Some(existing) = model::alive_settlement(&tx, salesperson_id, period_code)
            .await
            .map_err(|error| utils::db_error(error, "查询已有结算单"))?
        {
            if existing.is_posted() {
                return Ok(Err(format!(
                    "结算单 {} 已「{}」入账，不能重算（如需更正请红冲凭证后作废该单）",
                    existing.settlement_no,
                    commission_dto::status_label(&existing.status)
                )));
            }
            model::soft_delete(&tx, existing.id, operator, now)
                .await
                .map_err(|error| utils::db_error(error, "软删旧草稿结算单"))?;
            tracing::info!(
                settlement_no = %existing.settlement_no,
                "佣金计提：旧草稿已软删，按本期数重算"
            );
        }

        let candidates = model::load_candidates(&tx, salesperson_id)
            .await
            .map_err(|error| utils::db_error(error, "查询计提候选应收"))?;
        let rules = model::load_rules(&tx)
            .await
            .map_err(|error| utils::db_error(error, "查询佣金规则"))?;
        let mut so_ids: Vec<Uuid> = candidates.iter().map(|c| c.so_id).collect();
        so_ids.sort();
        so_ids.dedup();
        let lines = model::load_order_lines(&tx, &so_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询订单明细"))?;
        let date = commission_dto::period_end_date(period_code)
            .ok_or_else(|| bad_request(format!("period_code 非法：{period_code}")))?;

        let mut items: Vec<NewItem> = Vec::new();
        let mut total_base = Decimal::ZERO;
        for candidate in &candidates {
            let delta = candidate.delta();
            if delta == Decimal::ZERO {
                continue;
            }
            let order_lines: Vec<model::OrderLineRow> = lines
                .iter()
                .filter(|line| line.so_id == candidate.so_id)
                .cloned()
                .collect();
            for plan_line in plan_receivable(
                candidate.receivable_id,
                candidate.so_id,
                delta,
                &order_lines,
                &rules,
                salesperson_id,
                date,
            ) {
                total_base += plan_line.base_amount;
                items.push(NewItem {
                    receivable_id: plan_line.receivable_id,
                    so_id: plan_line.so_id,
                    rule_id: Some(plan_line.rule_id),
                    base_amount: plan_line.base_amount,
                    rate: plan_line.rate,
                    commission: plan_line.commission,
                    remark: plan_line.remark,
                });
            }
        }

        if items.is_empty() {
            return Ok(Err(
                "本期没有可计佣的已收款（未收款 / 已全部计过 / 未命中生效规则）".to_string(),
            ));
        }
        let total_commission: Decimal = items.iter().map(|item| item.commission).sum();
        // ck_biz_cmset_total_sales 要求非负：纯冲回期间记 0，冲回看 total_commission（负）
        let total_sales = if total_base > Decimal::ZERO {
            total_base.round_dp(BASE_DP)
        } else {
            Decimal::ZERO
        };
        let settlement_no = model::next_settlement_no(&tx, period_code)
            .await
            .map_err(|error| utils::db_error(error, "取佣金结算单号"))?;
        let id = model::insert_settlement(
            &tx,
            &settlement_no,
            salesperson_id,
            period_code,
            total_sales,
            total_commission.round_dp(MONEY_DP),
            remark,
            operator,
            now,
        )
        .await
        .map_err(|error| utils::db_error(error, "写佣金结算单"))?;
        model::insert_items(&tx, id, &items, now)
            .await
            .map_err(|error| utils::db_error(error, "写佣金明细"))?;
        tracing::info!(
            settlement_no = %settlement_no,
            salesperson_id = %salesperson_id,
            period = %period_code,
            items = items.len(),
            total_sales = %total_sales,
            total_commission = %total_commission.round_dp(MONEY_DP),
            "佣金计提：结算单已生成（DRAFT）"
        );
        let detail = detail_in(&tx, id).await?;
        Ok(Ok(detail.settlement))
    }
    .await;
    match result {
        Ok(Ok(res)) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交佣金计提事务"))?;
            Ok(Ok(res))
        }
        Ok(Err(reason)) => {
            // 没有东西可写：回滚（含上面可能已做的软删 —— 不能让「重算落空」把旧草稿弄丢）
            tx.rollback()
                .await
                .map_err(|error| utils::db_error(error, "回滚佣金计提事务"))?;
            Ok(Err(reason))
        }
        Err(err) => Err(err),
    }
}

// ==================== 查询 ====================

/// 结算单详情（同事务内取，供计提返回体复用）。
async fn detail_in(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<SettlementDetailRes, ApiError> {
    let row = model::find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "读佣金结算单"))?
        .ok_or_else(|| not_found(format!("佣金结算单 {id} 不存在或已删除")))?;
    let name = salesperson_name_in(ex, row.salesperson_id).await?;
    let voucher_no = match row.voucher_id {
        Some(voucher_id) => model::voucher_no_of(ex, voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "读佣金凭证号"))?,
        None => None,
    };
    let rows = model::items_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "读佣金明细"))?;
    let so_ids: Vec<Uuid> = rows.iter().filter_map(|item| item.so_id).collect();
    let receivable_ids: Vec<Uuid> = rows.iter().filter_map(|item| item.receivable_id).collect();
    let so_nos = model::order_nos(ex, &so_ids)
        .await
        .map_err(|error| utils::db_error(error, "读订单号"))?;
    let ar_nos = model::receivable_nos(ex, &receivable_ids)
        .await
        .map_err(|error| utils::db_error(error, "读应收单号"))?;
    let rule_names = rule_names_in(ex).await?;
    let items = rows
        .iter()
        .map(|item| CommissionItemRes {
            id: item.id.to_string(),
            so_id: item.so_id.map(|id| id.to_string()),
            so_no: item.so_id.and_then(|id| so_nos.get(&id).cloned()),
            receivable_id: item.receivable_id.map(|id| id.to_string()),
            ar_no: item.receivable_id.and_then(|id| ar_nos.get(&id).cloned()),
            rule_id: item.rule_id.map(|id| id.to_string()),
            rule_name: item.rule_id.and_then(|id| rule_names.get(&id).cloned()),
            base_amount: item.base_amount,
            rate: item.rate,
            commission: item.commission,
            remark: item.remark.clone(),
        })
        .collect();
    Ok(SettlementDetailRes {
        settlement: SettlementRes {
            id: row.id.to_string(),
            settlement_no: row.settlement_no,
            salesperson_id: row.salesperson_id.to_string(),
            salesperson_name: name,
            period_code: row.period_code,
            total_sales: row.total_sales,
            total_commission: row.total_commission,
            status: row.status,
            paid_at: row.paid_at,
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            voucher_no,
            item_count: i64::try_from(rows.len()).unwrap_or(0),
            remark: row.remark,
            created_at: row.created_at,
        },
        items,
    })
}

async fn salesperson_name_in(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<String, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT COALESCE(NULLIF(real_name, ''), username, '') AS name \
               FROM sys_user WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "读销售员姓名"))?;
    match row {
        Some(row) => row
            .get_typed::<String>("name")
            .map_err(|error| utils::db_error(error, "读销售员姓名")),
        None => Ok(String::new()),
    }
}

async fn rule_names_in(
    ex: &impl SqlExecutor,
) -> std::result::Result<std::collections::HashMap<Uuid, String>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT id, rule_name FROM biz_commission_rule".to_string(),
            vec![],
        )
        .await
        .map_err(|error| utils::db_error(error, "读佣金规则名"))?;
    let mut map = std::collections::HashMap::new();
    for row in &rows {
        let id: Uuid = row
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读规则 id"))?;
        let name: String = row
            .get_typed("rule_name")
            .map_err(|error| utils::db_error(error, "读规则名"))?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 结算单详情（HTTP 入口用）。
pub async fn detail(id: &str) -> std::result::Result<SettlementDetailRes, ApiError> {
    let id = commission_dto::parse_uuid(id, "id")?;
    detail_in(db(), id).await
}

/// 结算单分页（数据权限：销售员只看自己的）。
pub async fn page(
    principal: &ScopedPrincipal,
    query: &CommissionPageQuery,
) -> std::result::Result<
    (
        (Vec<(model::SettlementRow, String, i64)>, i64),
        CommissionFilter,
    ),
    ApiError,
> {
    query.validate()?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    let filter = CommissionFilter {
        page,
        page_size,
        period_code: query
            .period_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        salesperson_id: match query.salesperson_id.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => {
                Some(commission_dto::parse_uuid(raw, "salesperson_id")?)
            }
            _ => None,
        },
        status: query
            .status
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        keyword: query
            .keyword
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
    };
    let rows = model::page(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询佣金结算单"))?;
    Ok((rows, filter))
}

// ==================== 状态迁移（审批 / 发放） ====================

/// 审批 / 发放：`DRAFT → APPROVED`（生成计提凭证）、`APPROVED → PAID`（生成付款凭证）。
pub async fn change_status(
    operator: Option<Uuid>,
    id: &str,
    req: CommissionStatusReq,
) -> std::result::Result<SettlementDetailRes, ApiError> {
    let id = commission_dto::parse_uuid(id, "id")?;
    let target = req.status.trim().to_string();
    if !STATUSES.contains(&target.as_str()) {
        return Err(bad_request(format!(
            "status 只认 {STATUSES:?}（收到「{target}」）"
        )));
    }
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启佣金状态迁移事务"))?;
    let result = async {
        let row = model::lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定佣金结算单"))?
            .ok_or_else(|| not_found(format!("佣金结算单 {id} 不存在或已删除")))?;
        if row.status == target {
            return Err(conflict(format!(
                "结算单 {} 已经是「{}」",
                row.settlement_no,
                commission_dto::status_label(&target)
            )));
        }
        if !commission_dto::LEGAL_EDGES
            .iter()
            .any(|(from, to)| *from == row.status && *to == target)
        {
            return Err(conflict(format!(
                "结算单 {} 不能从「{}」变成「{}」（合法路径：DRAFT → APPROVED → PAID）",
                row.settlement_no,
                commission_dto::status_label(&row.status),
                commission_dto::status_label(&target)
            )));
        }
        let mut voucher_id = None;
        let paid_at = match target.as_str() {
            STATUS_APPROVED => {
                period::ensure_period_writable(&tx, &row.period_code, "佣金计提入账").await?;
                voucher_id = post_accrual_voucher(&tx, &row, operator, now).await?;
                None
            }
            STATUS_PAID => Some(now),
            _ => None,
        };
        if target == STATUS_PAID {
            let payment = post_payment_voucher(&tx, &row, operator, now).await?;
            tracing::info!(
                settlement_no = %row.settlement_no,
                voucher_id = ?payment,
                "佣金发放：付款凭证已生成（借 2241.02 / 贷 1002）"
            );
        }
        let affected = model::apply_status(
            &tx,
            row.id,
            &row.status,
            &target,
            paid_at,
            voucher_id,
            reason.as_deref(),
            operator,
            now,
        )
        .await
        .map_err(|error| utils::db_error(error, "写佣金结算单状态"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "结算单 {} 的状态在本次操作期间被改动（期望 {}），请刷新后重试",
                row.settlement_no, row.status
            )));
        }
        detail_in(&tx, row.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交佣金状态迁移"))?;
            Ok(detail)
        }
        Err(err) => Err(err),
    }
}

/// 计提凭证：借 5601 / 贷 2241.02（佣金为负时方向相反）。返回凭证 id。
async fn post_accrual_voucher(
    ex: &impl SqlExecutor,
    row: &model::SettlementRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    write_commission_voucher(ex, row, CommissionVoucherKind::Accrual, operator, now).await
}

/// 发放凭证：借 2241.02 / 贷 1002（佣金为负时方向相反）。
async fn post_payment_voucher(
    ex: &impl SqlExecutor,
    row: &model::SettlementRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    write_commission_voucher(ex, row, CommissionVoucherKind::Payment, operator, now).await
}

/// 佣金凭证的两种时点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommissionVoucherKind {
    /// 计提：借费用 / 贷应付。
    Accrual,
    /// 发放：借应付 / 贷银行。
    Payment,
}

impl CommissionVoucherKind {
    fn summary_prefix(self) -> &'static str {
        match self {
            CommissionVoucherKind::Accrual => "佣金计提",
            CommissionVoucherKind::Payment => "佣金发放",
        }
    }

    fn remark(self) -> &'static str {
        match self {
            CommissionVoucherKind::Accrual => {
                "佣金计提：借 5601 销售费用 / 贷 2241.02 其他应付款-佣金（ARCH-7 §15.4.3 ④）"
            }
            CommissionVoucherKind::Payment => {
                "佣金发放：借 2241.02 其他应付款-佣金 / 贷 1002 银行存款（ARCH-7 §15.4.3 ⑤）"
            }
        }
    }
}

/// 两张凭证共用的一处实现（分录方向按金额正负翻转，借贷必然平衡）。
async fn write_commission_voucher(
    ex: &impl SqlExecutor,
    row: &model::SettlementRow,
    kind: CommissionVoucherKind,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let amount = row.total_commission.abs().round_dp(MONEY_DP);
    if amount <= Decimal::ZERO {
        tracing::info!(
            settlement_no = %row.settlement_no,
            "佣金凭证：合计为 0，不出凭证（零金额凭证过不了 post_voucher）"
        );
        return Ok(None);
    }
    let date = commission_dto::period_end_date(&row.period_code)
        .ok_or_else(|| bad_request(format!("period_code 非法：{}", row.period_code)))?;
    let expense = voucher::account_required(ex, model::ACCOUNT_EXPENSE, "销售费用")
        .await
        .map_err(|error| utils::db_error(error, "取销售费用科目 5601"))?;
    let payable = voucher::account_required(ex, model::ACCOUNT_PAYABLE, "其他应付款-佣金")
        .await
        .map_err(|error| utils::db_error(error, "取其他应付款-佣金科目 2241.02"))?;
    let bank = voucher::account_required(ex, model::ACCOUNT_BANK, "银行存款")
        .await
        .map_err(|error| utils::db_error(error, "取银行存款科目 1002"))?;

    // 正数：正常方向；负数（纯冲回）：整张凭证反向，借仍是「费用/应付」那一侧的最小集合
    let (debit, credit, debit_summary, credit_summary) =
        match (kind, row.total_commission < Decimal::ZERO) {
            (CommissionVoucherKind::Accrual, false) => {
                (expense, payable, "计提销售佣金", "应付销售佣金")
            }
            (CommissionVoucherKind::Accrual, true) => {
                (payable, expense, "冲回应付佣金", "冲回销售费用")
            }
            (CommissionVoucherKind::Payment, false) => (payable, bank, "发放销售佣金", "银行付款"),
            (CommissionVoucherKind::Payment, true) => {
                (bank, payable, "收回多付佣金", "冲回应付佣金")
            }
        };
    let summary = format!(
        "{} {}（销售员 {} 期间 {}）",
        kind.summary_prefix(),
        row.settlement_no,
        salesperson_name_in(ex, row.salesperson_id).await?,
        row.period_code
    );
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: format!("{summary} {debit_summary}"),
            account: debit,
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: format!("{summary} {credit_summary}"),
            account: credit,
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    let voucher_id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: crate::business::shared::doc_kind::VoucherSourceType::Commission.as_str(),
            source_id: Some(row.id),
            source_no: Some(row.settlement_no.as_str()),
            voucher_date: date,
            summary,
            lines,
            remark: Some(kind.remark()),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| utils::db_error(error, "生成佣金凭证"))?;
    Ok(Some(voucher_id))
}

// ==================== HTTP 入口 ====================

/// 生成结算单。
#[nik::permission(
    path = "/business/erp/commission/settle",
    method = "POST",
    permission = "business:erp:commission-settle:create"
)]
#[nik::post("/business/erp/commission/settle")]
pub async fn commission_settle_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SettleReq>,
) -> ResResult<SettleRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(settle(operator, req).await?))
}

/// 结算单分页。
#[nik::permission(
    path = "/business/erp/commission/page",
    method = "GET",
    permission = "business:erp:commission-settle:list"
)]
#[nik::get("/business/erp/commission/page")]
pub async fn commission_page_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<CommissionPageQuery>,
) -> ResResult<PageData<Vec<SettlementRes>>> {
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let ((rows, total), filter) = page(&principal, &query).await?;
    let mut list = Vec::with_capacity(rows.len());
    for (row, name, item_count) in rows {
        let voucher_no = match row.voucher_id {
            Some(voucher_id) => model::voucher_no_of(db(), voucher_id)
                .await
                .map_err(|error| utils::db_error(error, "读佣金凭证号"))?,
            None => None,
        };
        list.push(SettlementRes {
            id: row.id.to_string(),
            settlement_no: row.settlement_no,
            salesperson_id: row.salesperson_id.to_string(),
            salesperson_name: name,
            period_code: row.period_code,
            total_sales: row.total_sales,
            total_commission: row.total_commission,
            status: row.status,
            paid_at: row.paid_at,
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            voucher_no,
            item_count,
            remark: row.remark,
            created_at: row.created_at,
        });
    }
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        filter.page,
        filter.page_size,
    ))
}

/// 结算单详情。
#[nik::permission(
    path = "/business/erp/commission/{id}",
    method = "GET",
    permission = "business:erp:commission-settle:list"
)]
#[nik::get("/business/erp/commission/{id}")]
pub async fn commission_detail_api(Path(id): Path<String>) -> ResResult<SettlementDetailRes> {
    Ok(Res::ok(detail(&id).await?))
}

/// 审批 / 发放。
#[nik::permission(
    path = "/business/erp/commission/{id}/status",
    method = "PATCH",
    permission = "business:erp:commission-settle:status"
)]
#[nik::patch("/business/erp/commission/{id}/status")]
pub async fn commission_status_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CommissionStatusReq>,
) -> ResResult<SettlementDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(change_status(operator, &id, req).await?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::commission_model::OrderLineRow;
    use nik::prelude::rust_decimal::prelude::FromPrimitive;

    fn dec(value: f64) -> Decimal {
        Decimal::from_f64(value).expect("小数可转 Decimal")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期")
    }

    fn rule(
        scope_type: &str,
        scope_id: Option<Uuid>,
        calc_type: &str,
        rate: Decimal,
        min: Decimal,
        max: Option<Decimal>,
        from: &str,
    ) -> RuleRow {
        RuleRow {
            id: Uuid::now_v7(),
            rule_name: format!("{scope_type}/{calc_type}/{rate}"),
            scope_type: scope_type.to_string(),
            scope_id,
            calc_type: calc_type.to_string(),
            rate,
            min_amount: min,
            max_amount: max,
            effective_from: date(from),
            effective_to: None,
            created_at: Utc::now(),
        }
    }

    fn line(so_id: Uuid, material: Uuid, amount: Decimal, qty: Decimal) -> OrderLineRow {
        OrderLineRow {
            so_id,
            material_id: Some(material),
            amount,
            qty,
        }
    }

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_v0310() {
        assert_eq!(PERM_CREATE, "business:erp:commission-settle:create");
        assert_eq!(PERM_LIST, "business:erp:commission-settle:list");
        assert_eq!(PERM_STATUS, "business:erp:commission-settle:status");
        for code in COMMISSION_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business");
            assert_eq!(parts[1], "erp");
            assert!(parts[2].starts_with("commission-"));
        }
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/commission/settle",
            "/business/erp/commission/page",
            "/business/erp/commission/{id}",
            "/business/erp/commission/{id}/status",
        ] {
            assert!(path.starts_with("/business/erp/commission"));
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀");
        }
    }

    // ---------- 规则优先级 ----------

    #[test]
    fn salesperson_rule_beats_material_and_global() {
        let sp = Uuid::now_v7();
        let material = Uuid::now_v7();
        let rules = vec![
            rule(
                SCOPE_ALL,
                None,
                CALC_PERCENT,
                dec(5.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
            rule(
                SCOPE_MATERIAL,
                Some(material),
                CALC_PERCENT,
                dec(8.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
            rule(
                SCOPE_SALESPERSON,
                Some(sp),
                CALC_PERCENT,
                dec(12.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
        ];
        let picked = pick_rule(&rules, sp, Some(material), date("2026-11-30")).unwrap();
        assert_eq!(picked.rate, dec(12.0));
    }

    #[test]
    fn material_rule_beats_global() {
        let sp = Uuid::now_v7();
        let material = Uuid::now_v7();
        let rules = vec![
            rule(
                SCOPE_ALL,
                None,
                CALC_PERCENT,
                dec(5.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
            rule(
                SCOPE_MATERIAL,
                Some(material),
                CALC_PERCENT,
                dec(8.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
        ];
        assert_eq!(
            pick_rule(&rules, sp, Some(material), date("2026-11-30"))
                .unwrap()
                .rate,
            dec(8.0)
        );
        // 别的物料只命中全局
        assert_eq!(
            pick_rule(&rules, sp, Some(Uuid::now_v7()), date("2026-11-30"))
                .unwrap()
                .rate,
            dec(5.0)
        );
        // 订单没有明细（无物料）时，物料规则不参与匹配
        assert_eq!(
            pick_rule(&rules, sp, None, date("2026-11-30"))
                .unwrap()
                .rate,
            dec(5.0)
        );
    }

    #[test]
    fn salesperson_rule_of_other_person_is_ignored() {
        let sp = Uuid::now_v7();
        let other = Uuid::now_v7();
        let rules = vec![
            rule(
                SCOPE_ALL,
                None,
                CALC_PERCENT,
                dec(5.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
            rule(
                SCOPE_SALESPERSON,
                Some(other),
                CALC_PERCENT,
                dec(30.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
        ];
        assert_eq!(
            pick_rule(&rules, sp, None, date("2026-11-30"))
                .unwrap()
                .rate,
            dec(5.0)
        );
    }

    #[test]
    fn effective_window_bounds_are_inclusive() {
        let sp = Uuid::now_v7();
        let mut only_nov = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(6.0),
            Decimal::ZERO,
            None,
            "2026-11-01",
        );
        only_nov.effective_to = Some(date("2026-11-30"));
        assert!(pick_rule(&[only_nov.clone()], sp, None, date("2026-11-01")).is_some());
        assert!(pick_rule(&[only_nov.clone()], sp, None, date("2026-11-30")).is_some());
        assert!(pick_rule(&[only_nov.clone()], sp, None, date("2026-10-31")).is_none());
        assert!(pick_rule(&[only_nov], sp, None, date("2026-12-01")).is_none());
    }

    #[test]
    fn same_scope_tie_break_prefers_newer_effective_from() {
        let sp = Uuid::now_v7();
        let mut old = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(5.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        );
        old.effective_to = Some(date("2026-11-30"));
        let new = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(7.0),
            Decimal::ZERO,
            None,
            "2026-11-01",
        );
        assert_eq!(
            pick_rule(&[old, new], sp, None, date("2026-11-30"))
                .unwrap()
                .rate,
            dec(7.0)
        );
    }

    // ---------- 佣金计算 ----------

    #[test]
    fn percent_commission_is_base_times_rate() {
        let rule = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(3.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        );
        let (rate, commission) = commission_of(&rule, dec(10000.0)).unwrap();
        assert_eq!(rate, dec(3.0));
        assert_eq!(commission, dec(300.0));
    }

    #[test]
    fn fixed_commission_ignores_base() {
        let rule = rule(
            SCOPE_ALL,
            None,
            CALC_FIXED,
            dec(200.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        );
        assert_eq!(commission_of(&rule, dec(10000.0)).unwrap().1, dec(200.0));
        assert_eq!(commission_of(&rule, dec(1.0)).unwrap().1, dec(200.0));
    }

    #[test]
    fn min_amount_skips_small_base() {
        let rule = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(10.0),
            dec(1000.0),
            None,
            "2026-01-01",
        );
        assert!(commission_of(&rule, dec(999.99)).is_none());
        assert_eq!(commission_of(&rule, dec(1000.0)).unwrap().1, dec(100.0));
    }

    #[test]
    fn max_amount_caps_commission() {
        let rule = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(10.0),
            Decimal::ZERO,
            Some(dec(50.0)),
            "2026-01-01",
        );
        assert_eq!(commission_of(&rule, dec(10000.0)).unwrap().1, dec(50.0));
        // 未超封顶时保持原值
        assert_eq!(commission_of(&rule, dec(400.0)).unwrap().1, dec(40.0));
    }

    #[test]
    fn clawback_is_negative_and_symmetric() {
        let percent = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(5.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        );
        let (_, commission) = commission_of(&percent, dec(-1000.0)).unwrap();
        assert_eq!(commission, dec(-50.0));
        // 冲回同样受封顶限制（否则冲多了等于反向发钱）
        let capped = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(10.0),
            Decimal::ZERO,
            Some(dec(20.0)),
            "2026-01-01",
        );
        assert_eq!(commission_of(&capped, dec(-10000.0)).unwrap().1, dec(-20.0));
    }

    #[test]
    fn commission_rounds_to_cent() {
        let rule = rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(3.3333),
            Decimal::ZERO,
            None,
            "2026-01-01",
        );
        // 1000 * 3.3333% = 33.333 → 33.33
        assert_eq!(commission_of(&rule, dec(1000.0)).unwrap().1, dec(33.33));
    }

    // ---------- 基数分摊 ----------

    #[test]
    fn allocate_sums_exactly_to_base() {
        let shares = allocate(dec(100.0), &[dec(1.0), dec(1.0), dec(1.0)]);
        let sum: Decimal = shares.iter().copied().sum();
        assert_eq!(sum, dec(100.0));
        assert_eq!(shares[2], dec(33.3334), "末桶吸收残差");
    }

    #[test]
    fn allocate_falls_back_to_equal_split_when_weights_are_zero() {
        let shares = allocate(dec(10.0), &[Decimal::ZERO, Decimal::ZERO]);
        assert_eq!(shares, vec![dec(5.0), dec(5.0)]);
    }

    #[test]
    fn allocate_without_weights_returns_whole_base() {
        assert_eq!(allocate(dec(88.88), &[]), vec![dec(88.88)]);
    }

    #[test]
    fn allocate_keeps_sign_for_clawback() {
        let shares = allocate(dec(-90.0), &[dec(1.0), dec(2.0)]);
        assert_eq!(shares[0], dec(-30.0));
        assert_eq!(shares[1], dec(-60.0));
        assert_eq!(shares.iter().copied().sum::<Decimal>(), dec(-90.0));
    }

    // ---------- 计划（按物料拆 + 规则优先级 + 未收款不计） ----------

    #[test]
    fn plan_splits_by_material_and_uses_best_rule() {
        let sp = Uuid::now_v7();
        let so = Uuid::now_v7();
        let ar = Uuid::now_v7();
        let m1 = Uuid::now_v7();
        let m2 = Uuid::now_v7();
        let rules = vec![
            rule(
                SCOPE_ALL,
                None,
                CALC_PERCENT,
                dec(2.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
            rule(
                SCOPE_MATERIAL,
                Some(m1),
                CALC_PERCENT,
                dec(5.0),
                Decimal::ZERO,
                None,
                "2026-01-01",
            ),
        ];
        let lines = vec![
            line(so, m1, dec(600.0), dec(6.0)),
            line(so, m2, dec(400.0), dec(4.0)),
        ];
        let plan = plan_receivable(ar, so, dec(1000.0), &lines, &rules, sp, date("2026-11-30"));
        assert_eq!(plan.len(), 2);
        let m1_line = plan.iter().find(|p| p.material_id == Some(m1)).unwrap();
        let m2_line = plan.iter().find(|p| p.material_id == Some(m2)).unwrap();
        assert_eq!(m1_line.base_amount, dec(600.0));
        assert_eq!(m1_line.commission, dec(30.0)); // 600 × 5%
        assert_eq!(m2_line.base_amount, dec(400.0));
        assert_eq!(m2_line.commission, dec(8.0)); // 400 × 2%（只有全局规则）
        assert_eq!(
            m1_line.base_amount + m2_line.base_amount,
            dec(1000.0),
            "分摊之和必须等于本期基数"
        );
    }

    #[test]
    fn plan_is_empty_when_nothing_received() {
        // 未收款 → delta = 0 → 一分佣金都不计（卡面硬要求：按已收款计佣）
        let sp = Uuid::now_v7();
        let so = Uuid::now_v7();
        let rules = vec![rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(5.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        )];
        let lines = vec![line(so, Uuid::now_v7(), dec(1000.0), dec(1.0))];
        assert!(
            plan_receivable(
                Uuid::now_v7(),
                so,
                Decimal::ZERO,
                &lines,
                &rules,
                sp,
                date("2026-11-30")
            )
            .is_empty()
        );
    }

    #[test]
    fn plan_is_empty_without_any_rule() {
        let so = Uuid::now_v7();
        let lines = vec![line(so, Uuid::now_v7(), dec(1000.0), dec(1.0))];
        assert!(
            plan_receivable(
                Uuid::now_v7(),
                so,
                dec(1000.0),
                &lines,
                &[],
                Uuid::now_v7(),
                date("2026-11-30")
            )
            .is_empty()
        );
    }

    #[test]
    fn plan_marks_clawback_rows() {
        let sp = Uuid::now_v7();
        let so = Uuid::now_v7();
        let rules = vec![rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(5.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        )];
        let lines = vec![line(so, Uuid::now_v7(), dec(1000.0), dec(1.0))];
        let plan = plan_receivable(
            Uuid::now_v7(),
            so,
            dec(-1000.0),
            &lines,
            &rules,
            sp,
            date("2026-11-30"),
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].base_amount, dec(-1000.0));
        assert_eq!(plan[0].commission, dec(-50.0));
        assert!(plan[0].remark.as_deref().unwrap().contains("冲回"));
    }

    #[test]
    fn plan_without_order_lines_falls_back_to_global_rule() {
        // 订单没有明细（历史脏数据）：只能吃销售员 / 全局规则，不能静默丢单
        let sp = Uuid::now_v7();
        let so = Uuid::now_v7();
        let rules = vec![rule(
            SCOPE_ALL,
            None,
            CALC_PERCENT,
            dec(4.0),
            Decimal::ZERO,
            None,
            "2026-01-01",
        )];
        let plan = plan_receivable(
            Uuid::now_v7(),
            so,
            dec(500.0),
            &[],
            &rules,
            sp,
            date("2026-11-30"),
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].commission, dec(20.0));
    }
}
