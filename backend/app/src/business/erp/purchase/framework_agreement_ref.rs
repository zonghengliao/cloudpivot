//! 框架协议「订单引用」：校验（价格 / 数量 / 金额）+ 执行量回写（M4-08）。
//!
//! 表结构依据：`backend/app/migrations/business/V0450__framework_agreement.up.sql`（协议两表）
//! 与 `V0451__framework_agreement_order_ref.up.sql`（订单侧引用列）。
//! 协议侧的口径（有效期判据 / 容差判据 / 上限语义）一律复用
//! [`framework_agreement_model`]，本文件**不重写**第二套判据。
//!
//! | 入口 | 用途 |
//! |------|------|
//! | [`resolve`] | 建单 / 改单：锁协议行 → 判可用性 → 判单价 → 判累计量/额，返回逐行的协议行 id |
//! | [`refresh_executed`] | 下单 / 作废 / 删除之后，按单据链把 `executed_qty` / `executed_amount` **重算**回来 |
//! | [`release_order`] | 作废 / 删除时的入口：把这张单引用的协议行全部重算一遍（额度释放） |
//! | [`ref_of_order`] | 详情出参：这张单挂的是哪份协议（现查现显，不落快照） |
//!
//! # 五条硬口径（写在这里，免得下游各写一套）
//!
//! ① **引用是可选的**：`framework_agreement_id IS NULL` = 本单不挂协议 —— 零星 / 紧急 /
//!    协议外物料都能下单，NULL 时本模块一行都不执行（不校验价格、不占额度）。
//! ② **校验看「现算的累计量」，不看回写列**：判据是「同一协议行上所有**未作废且未删除**
//!    采购单的 `Σ qty` + 本单」，从来不读 `executed_qty`。理由与合同额度守卫
//!    （`purchase_handler::guard_contract_amount` 里那句「缓存必漂移」）同源：
//!    回写列是给人看的快照，拿它做准入判据，漏刷一次就会误拒或误放。
//! ③ **回写是「重算」不是「累加」**：`+=` 一旦被重试 / 回滚 / 并发咬到一次就永久偏；
//!    [`refresh_executed`] 每次按单据链重算成绝对值，天然幂等，重复调用不产生偏差。
//! ④ **并发靠协议行锁**：`resolve` 第一句 [`framework_agreement_model::lock_agreement`]
//!    （`SELECT … FOR UPDATE`），锁在事务提交前一直持有 —— 两个并发下单在同一份协议上排队，
//!    否则都读到「还剩 100」再各自放行一次就是超卖。
//!    锁顺序固定「合同行 → 协议行」（建单 / 改单都先锁合同，再进本模块），单向不交叉。
//! ⑤ **口径统一**：采购单行价 `price` 恒为**不含税**单价（库列 + 金额口径 A/B 都归一到它），
//!    所以比价前必须把协议价折成不含税（协议价是否含税看单头 `price_includes_tax`，
//!    税率用协议行的 `tax_rate`，**0–1 分数**，与采购单行的**百分数**税率不是一回事）。
//!    金额上限比较时口径跟协议价走：含税协议比 `Σ with_tax_amount`，不含税协议比 `Σ amount`。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只按主键读写采购单 / 协议（校验与回写），不做列表查询，
//!        不涉及数据范围；订单可见性由 business:erp:purchase-order:* 控制。
//! ```

use std::collections::HashMap;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::framework_agreement_model as fwa;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 采购单的结算币种。
///
/// 库列 `biz_purchase_order.currency_code CHAR(3) DEFAULT 'CNY'`，建单 / 改单的请求体里
/// **没有**币种字段（多币种采购尚未开放），所以挂协议的采购单恒为 CNY。
/// 协议价币种不是 CNY 时不能引用 —— 跨币种比价没有意义（汇率谁定、按哪天，都不在协议里）。
pub const ORDER_CURRENCY: &str = "CNY";

/// 一行待校验的下单明细（由 handler 从 `NewItem` 映射）。
#[derive(Debug, Clone)]
pub struct RefLine {
    /// 行号（1 起，报文里按「第 N 行」展示）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 数量（4 位标度）。
    pub qty: Decimal,
    /// 不含税单价（6 位）。
    pub price_excl: Decimal,
    /// 不含税金额（与 `Σ` 金额上限同口径）。
    pub amount: Decimal,
    /// 价税合计（协议价含税时用它比金额上限）。
    pub with_tax_amount: Decimal,
}

/// 一次引用的请求（单头上下文 + 全部明细行）。
#[derive(Debug, Clone)]
pub struct RefRequest<'a> {
    /// 协议主键。
    pub agreement_id: Uuid,
    /// 采购单供应商（必须等于协议的供应商）。
    pub supplier_id: Uuid,
    /// 下单日期（判有效期的基准日 —— **不是**今天：补录历史单要按历史那天判）。
    pub order_date: NaiveDate,
    /// 本单结算币种（见 [`ORDER_CURRENCY`]）。
    pub currency_code: &'a str,
    /// 明细（顺序即 `line_no`）。
    pub lines: &'a [RefLine],
}

/// 校验通过的结论：逐行对应的协议行 + 协议号（落单头快照）。
#[derive(Debug, Clone)]
pub struct RefResolution {
    /// 协议号（`FWA-yyyyMMdd-000001`），写进 `biz_purchase_order.framework_agreement_no_snapshot`。
    pub agreement_no: String,
    /// 与 [`RefRequest::lines`] **一一对应**的协议行 id（落 `biz_purchase_order_item`）。
    pub item_ids: Vec<Uuid>,
}

/// 一个协议行上的累计用量（现算）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    /// 累计数量。
    pub qty: Decimal,
    /// 累计不含税金额。
    pub amount: Decimal,
    /// 累计价税合计。
    pub with_tax: Decimal,
}

/// 详情出参用的协议引用（现查现显：名称 / 状态 / 条款会变，不落快照）。
#[derive(Debug, Clone)]
pub struct AgreementRefRow {
    /// 协议主键。
    pub id: Uuid,
    /// 协议号（协议已软删时回落到单头快照）。
    pub agreement_no: String,
    /// 协议名（协议已软删 → `None`）。
    pub agreement_name: Option<String>,
    /// 状态（协议已软删 → `None`）。
    pub status: Option<String>,
    /// 有效期起。
    pub start_date: Option<NaiveDate>,
    /// 有效期止。
    pub end_date: Option<NaiveDate>,
    /// 定价方式。
    pub price_mode: Option<String>,
    /// 协议价是否含税。
    pub price_includes_tax: Option<bool>,
    /// 协议价币种。
    pub price_currency: Option<String>,
    /// 容许上浮比例。
    pub price_tolerance: Option<Decimal>,
}

// ==================== 纯函数（判据，单测直接跑） ====================

/// 协议价折成**不含税**单价（6 位，四舍五入）。
///
/// * `price_includes_tax = false` ⇒ 原样（协议价本来就是不含税）；
/// * `price_includes_tax = true` ⇒ `agreed_price / (1 + tax_rate)`；
///   `tax_rate` 是**分数**（0.13 = 13%），与采购单行的百分数税率不同（约定 ⑤）。
///
/// 舍入用「四舍五入」而不是 `rust_decimal` 默认的「四舍六入五成双」：与
/// `purchase_model` 的金额口径（`MidpointAwayFromZero`）保持一致，避免同一个价在不同
/// 代码路径上差 1 个末位（0.000001 的差价看着无害，但判等 / 判超限时会翻结果）。
pub fn ex_tax_price(agreed_price: Decimal, includes_tax: bool, tax_rate: Decimal) -> Decimal {
    if !includes_tax {
        return agreed_price.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero);
    }
    let divisor = Decimal::ONE + tax_rate;
    if divisor.is_zero() {
        // 理论上进不来（`tax_rate >= 0` ⇒ divisor >= 1），留一条兜底而不是 panic
        return agreed_price.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero);
    }
    (agreed_price / divisor).round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero)
}

/// 本行允许的最高不含税单价：`协议价(不含税) × (1 + 容差)`。
pub fn price_cap(agreed_price: Decimal, includes_tax: bool, tax_rate: Decimal, tolerance: Decimal) -> Decimal {
    fwa::max_allowed_price(ex_tax_price(agreed_price, includes_tax, tax_rate), tolerance)
}

/// 金额上限的比较口径：协议价含税 ⇒ 比价税合计，否则比不含税金额。
pub fn amount_for_cap(amount: Decimal, with_tax: Decimal, includes_tax: bool) -> Decimal {
    if includes_tax { with_tax } else { amount }
}

// ==================== 读：现算累计用量 ====================

/// 某份协议下每个物料协议行的累计用量（跨单据现算）。
///
/// 口径（模块头 ②）：`Σ` 自「引用了该协议行的、未软删、未作废」的采购单明细。
/// * 草稿算（`DRAFT` 也是占额度的事实 —— 与合同额度守卫 `committed_amount` 同口径）；
/// * `SUBTRACTED` / `CLOSED` 算（关闭只是不再收货，量已经下出去了）；
/// * `CANCELLED` 不算（作废单不该继续吃额度）；
/// * `exclude_order` 排除自己（改单时本单的旧行还在库里，不排就被算两遍）。
pub async fn usage_of_agreement(
    ex: &impl SqlExecutor,
    agreement_id: Uuid,
    exclude_order: Option<Uuid>,
) -> DbResult<HashMap<Uuid, Usage>> {
    // SCOPE-EXEMPT: 按协议主键聚合，不涉及数据范围（见模块头「数据权限」）。
    let (filter, mut params) = match exclude_order {
        Some(order_id) => (
            " AND o.id <> ?::text::uuid",
            vec![Value::from(&agreement_id), Value::Text(order_id.to_string())],
        ),
        None => ("", vec![Value::from(&agreement_id)]),
    };
    params.shrink_to_fit();
    let rows = ex
        .query_sql(
            format!(
                "SELECT i.framework_agreement_item_id AS item_id, \
                        COALESCE(SUM(i.qty), 0) AS qty, \
                        COALESCE(SUM(i.amount), 0) AS amount, \
                        COALESCE(SUM(i.with_tax_amount), 0) AS with_tax \
                   FROM biz_purchase_order_item i \
                   JOIN biz_purchase_order o ON o.id = i.order_id \
                  WHERE o.deleted_at IS NULL \
                    AND o.status <> 'CANCELLED' \
                    AND i.framework_agreement_item_id IN ( \
                        SELECT it.id FROM {item_table} it WHERE it.agreement_id = ?::uuid ){filter} \
                  GROUP BY i.framework_agreement_item_id",
                item_table = fwa::ITEM_TABLE
            ),
            params,
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let item_id: Uuid = row.get_typed("item_id")?;
        map.insert(
            item_id,
            Usage {
                qty: row.get_typed("qty")?,
                amount: row.get_typed("amount")?,
                with_tax: row.get_typed("with_tax")?,
            },
        );
    }
    Ok(map)
}

/// 一张采购单引用了哪些协议行（去重；未挂协议 → 空）。
pub async fn referenced_items_of_order(ex: &impl SqlExecutor, order_id: Uuid) -> DbResult<Vec<Uuid>> {
    // SCOPE-EXEMPT: 按单据主键取引用列。
    let rows = ex
        .query_sql(
            "SELECT DISTINCT framework_agreement_item_id AS item_id \
               FROM biz_purchase_order_item \
              WHERE order_id = ?::uuid AND framework_agreement_item_id IS NOT NULL"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("item_id")).collect()
}

/// 详情出参：这张单挂的是哪份协议（现查；协议已软删时仍回落到单头快照，引用不丢）。
pub async fn ref_of_order(ex: &impl SqlExecutor, order_id: Uuid) -> DbResult<Option<AgreementRefRow>> {
    // SCOPE-EXEMPT: 按单据主键取引用。
    let rows = ex
        .query_sql(
            format!(
                "SELECT o.framework_agreement_id AS agreement_id, \
                        COALESCE(a.agreement_no, o.framework_agreement_no_snapshot, '') AS agreement_no, \
                        a.agreement_name, a.status, a.start_date, a.end_date, \
                        a.price_mode, a.price_includes_tax, a.price_currency, a.price_tolerance \
                   FROM biz_purchase_order o \
                   LEFT JOIN {table} a ON a.id = o.framework_agreement_id AND a.deleted_at IS NULL \
                  WHERE o.id = ?::uuid AND o.framework_agreement_id IS NOT NULL",
                table = fwa::AGREEMENT_TABLE
            ),
            vec![Value::from(&order_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(AgreementRefRow {
            id: row.get_typed("agreement_id")?,
            agreement_no: row.get_typed("agreement_no")?,
            agreement_name: row.get_typed("agreement_name")?,
            status: row.get_typed("status")?,
            start_date: row.get_typed("start_date")?,
            end_date: row.get_typed("end_date")?,
            price_mode: row.get_typed("price_mode")?,
            price_includes_tax: row.get_typed("price_includes_tax")?,
            price_currency: row.get_typed("price_currency")?,
            price_tolerance: row.get_typed("price_tolerance")?,
        })),
    }
}

// ==================== 校验（卡面的「校验价格/数量」+「超限拒绝」） ====================

/// 校验一张采购单能不能按这份协议下单，并给出逐行的协议行 id。
///
/// # 顺序（每一步都是「不通过就带着人话拒绝」）
///
/// ```text
/// ① 锁协议行（FOR UPDATE，并发下同一份协议排队）
/// ② 协议在不在 / 状态是不是 APPROVED / 下单日在不在有效期
/// ③ 供应商一致 / 币种一致
/// ④ 逐行：物料在不在协议范围（不在 → 400，报文点出行号与物料）
/// ⑤ 逐行：单价 ≤ 协议价 × (1 + 容差)（超 → 409）
/// ⑥ 逐协议行：累计量 + 本单 ≤ max_qty（超 → 409）；单头总量 / 总额同理
/// ```
///
/// # 为什么「先锁再读」
///
/// 第 ⑥ 步要读「别人已经下了多少」，读之前必须先锁协议单头：否则两个并发请求都读到
/// 同一份旧量，各自被放行一次 —— 协议量被超卖（与采购计划 `lock_plan` 同款理由）。
///
/// # 返回
///
/// 与 `req.lines` 一一对应的协议行 id + 协议号。`item_ids[i]` 就是第 `i` 行该落的
/// `framework_agreement_item_id`（客户端不许传，服务端按 `(协议, 物料)` 现查，见 V0451 头注释）。
pub async fn resolve(
    ex: &impl SqlExecutor,
    req: &RefRequest<'_>,
    exclude_order: Option<Uuid>,
) -> std::result::Result<RefResolution, ApiError> {
    // ---------- ① 锁协议行 ----------
    let locked = fwa::lock_agreement(ex, req.agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定框架协议"))?;
    if !locked {
        return Err(ApiError::new(
            400,
            error_code::ERP_FWA_NOT_APPLICABLE,
            format!("框架协议不存在或已删除（{}），请重新选择", req.agreement_id),
        ));
    }
    let agreement = fwa::find_agreement(ex, req.agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议"))?
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::ERP_FWA_NOT_APPLICABLE,
                format!("框架协议不存在或已删除（{}），请重新选择", req.agreement_id),
            )
        })?;

    // ---------- ② 可用性：状态 + 有效期 ----------
    if agreement.status != fwa::STATUS_APPROVED {
        return Err(ApiError::new(
            400,
            error_code::ERP_FWA_NOT_APPLICABLE,
            format!(
                "框架协议 {} 当前是「{}」（{}），只有审批通过（APPROVED）的协议可以下单 —— \
                 草稿能下单就等于没有审批",
                agreement.agreement_no,
                status_label(&agreement.status),
                agreement.status
            ),
        ));
    }
    if !agreement.is_effective_on(req.order_date) {
        return Err(ApiError::new(
            400,
            error_code::ERP_FWA_NOT_APPLICABLE,
            format!(
                "下单日期 {} 不在框架协议 {} 的有效期内（{} 至 {}，含边界）；\
                 到期请续签新协议，或用零星采购（不挂协议）下单",
                req.order_date, agreement.agreement_no, agreement.start_date, agreement.end_date
            ),
        ));
    }

    // ---------- ③ 供应商 / 币种一致 ----------
    if agreement.supplier_id != req.supplier_id {
        return Err(ApiError::new(
            400,
            error_code::ERP_FWA_NOT_APPLICABLE,
            format!(
                "框架协议 {} 的供应商是 {}（{}），与本单供应商 {} 不一致：\
                 协议价只对签约的那家供应商有效",
                agreement.agreement_no, agreement.supplier_display_name, agreement.supplier_id, req.supplier_id
            ),
        ));
    }
    if agreement.price_currency != req.currency_code {
        return Err(ApiError::new(
            400,
            error_code::ERP_FWA_NOT_APPLICABLE,
            format!(
                "框架协议 {} 的协议价币种是 {}，本单按 {} 结算：跨币种无法比对协议价，\
                 请改用同币种协议或走零星采购",
                agreement.agreement_no, agreement.price_currency, req.currency_code
            ),
        ));
    }

    // ---------- ④ 物料范围（逐行） ----------
    let items = fwa::items_of_agreement(ex, req.agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料范围"))?;
    let mut by_material: HashMap<Uuid, &fwa::AgreementItemRow> = HashMap::with_capacity(items.len());
    for item in &items {
        by_material.insert(item.material_id, item);
    }
    let mut resolved: Vec<&fwa::AgreementItemRow> = Vec::with_capacity(req.lines.len());
    for line in req.lines {
        let item = by_material.get(&line.material_id).copied().ok_or_else(|| {
            ApiError::new(
                400,
                error_code::ERP_FWA_NOT_APPLICABLE,
                format!(
                    "第 {} 行的物料 {} 不在框架协议 {} 的物料范围里（该协议覆盖 {} 个物料）：\
                     协议外物料请走零星采购（不挂协议），或先给协议补物料",
                    line.line_no,
                    line.material_id,
                    agreement.agreement_no,
                    items.len()
                ),
            )
        })?;
        resolved.push(item);
    }

    // ---------- ⑤ 单价（逐行） ----------
    for (line, item) in req.lines.iter().zip(resolved.iter()) {
        let cap = price_cap(
            item.agreed_price,
            agreement.price_includes_tax,
            item.tax_rate,
            agreement.price_tolerance,
        );
        if line.price_excl > cap {
            return Err(ApiError::new(
                409,
                error_code::ERP_FWA_PRICE_EXCEEDED,
                format!(
                    "第 {} 行（{} {}）不含税单价 {} 高于框架协议 {} 允许的上限 {}：\
                     协议价 {}（{}）× (1 + 容差 {}%) = {}。请改价，或由采购经理调整协议条款",
                    line.line_no,
                    item.material_code,
                    item.material_name,
                    line.price_excl,
                    agreement.agreement_no,
                    cap,
                    item.agreed_price,
                    if agreement.price_includes_tax {
                        "含税，已按税率折算"
                    } else {
                        "不含税"
                    },
                    (agreement.price_tolerance * Decimal::from(100)).round_dp(2),
                    cap
                ),
            ));
        }
    }

    // ---------- ⑥ 累计量 / 额（协议行级 + 单头级） ----------
    let usage = usage_of_agreement(ex, req.agreement_id, exclude_order)
        .await
        .map_err(|error| utils::db_error(error, "统计协议已执行量"))?;
    // 本单按协议行汇总（同一物料可能拆成多行：逐行判会被「两行各一半」绕过）
    let mut this_qty: HashMap<Uuid, Decimal> = HashMap::new();
    let mut this_amount = Decimal::ZERO;
    let mut this_with_tax = Decimal::ZERO;
    for (line, item) in req.lines.iter().zip(resolved.iter()) {
        *this_qty.entry(item.id).or_insert(Decimal::ZERO) += line.qty;
        this_amount += line.amount;
        this_with_tax += line.with_tax_amount;
    }
    for (line, item) in req.lines.iter().zip(resolved.iter()) {
        let Some(max_qty) = item.max_qty else { continue };
        let used = usage.get(&item.id).map(|u| u.qty).unwrap_or(Decimal::ZERO);
        let added = this_qty.get(&item.id).copied().unwrap_or(Decimal::ZERO);
        if used + added > max_qty {
            return Err(ApiError::new(
                409,
                error_code::ERP_FWA_QTY_EXCEEDED,
                format!(
                    "第 {} 行（{} {}）超出框架协议 {} 的数量上限：协议期内上限 {}，\
                     其它采购单已用 {}，本单该物料合计 {}，下单后累计 {} 将超上限（剩余 {}）",
                    line.line_no,
                    item.material_code,
                    item.material_name,
                    agreement.agreement_no,
                    max_qty,
                    used,
                    added,
                    used + added,
                    max_qty - used
                ),
            ));
        }
    }
    if let Some(total_max_qty) = agreement.total_max_qty {
        let used: Decimal = usage.values().map(|u| u.qty).sum();
        if used + this_qty.values().copied().sum::<Decimal>() > total_max_qty {
            return Err(ApiError::new(
                409,
                error_code::ERP_FWA_QTY_EXCEEDED,
                format!(
                    "本单超出框架协议 {} 的**总量**上限：协议总量 {}，其它采购单已用 {}，本单 {}，\
                     下单后累计 {} 将超上限",
                    agreement.agreement_no,
                    total_max_qty,
                    used,
                    this_qty.values().copied().sum::<Decimal>(),
                    used + this_qty.values().copied().sum::<Decimal>()
                ),
            ));
        }
    }
    if let Some(total_max_amount) = agreement.total_max_amount {
        let used: Decimal = usage
            .values()
            .map(|u| amount_for_cap(u.amount, u.with_tax, agreement.price_includes_tax))
            .sum();
        let this = amount_for_cap(this_amount, this_with_tax, agreement.price_includes_tax);
        if used + this > total_max_amount {
            return Err(ApiError::new(
                409,
                error_code::ERP_FWA_QTY_EXCEEDED,
                format!(
                    "本单超出框架协议 {} 的**总额**上限：协议总额 {}（{}口径），其它采购单已用 {}，本单 {}，\
                     下单后累计 {} 将超上限",
                    agreement.agreement_no,
                    total_max_amount,
                    if agreement.price_includes_tax {
                        "含税"
                    } else {
                        "不含税"
                    },
                    used,
                    this,
                    used + this
                ),
            ));
        }
    }

    Ok(RefResolution {
        agreement_no: agreement.agreement_no,
        item_ids: resolved.iter().map(|item| item.id).collect(),
    })
}

// ==================== 回写（执行量快照重算） ====================

/// 把若干协议行的 `executed_qty` / `executed_amount` **按单据链重算**（幂等）。
///
/// 两步 SQL 就够，不需要「读出来再写回去」：
/// 1. 有引用的行 → 直接赋成 `Σ`（绝对值，不是 `+=`）；
/// 2. 没有引用的行（引用它的单全作废 / 删了）→ 归零。
///
/// 重算完把涉及的协议单头两个派生合计刷一遍（[`fwa::recalc_totals`]）。
/// 失败一律向上抛：调用方在事务里，回滚比留一份错量安全。
pub async fn refresh_executed(ex: &impl SqlExecutor, item_ids: &[Uuid]) -> DbResult<()> {
    let mut ids: Vec<Uuid> = item_ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let array = format!("{{{}}}", ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(","));
    // SCOPE-EXEMPT: 按协议行主键重算派生列。
    ex.exec_sql(
        format!(
            "UPDATE {item_table} it \
                SET executed_qty = COALESCE(s.qty, 0), \
                    executed_amount = COALESCE(s.amount, 0), \
                    updated_at = now() \
               FROM (SELECT i.framework_agreement_item_id AS item_id, \
                            SUM(i.qty) AS qty, SUM(i.amount) AS amount \
                       FROM biz_purchase_order_item i \
                       JOIN biz_purchase_order o ON o.id = i.order_id \
                      WHERE i.framework_agreement_item_id = ANY(?::text::uuid[]) \
                        AND o.deleted_at IS NULL AND o.status <> 'CANCELLED' \
                      GROUP BY i.framework_agreement_item_id) s \
              WHERE it.id = s.item_id",
            item_table = fwa::ITEM_TABLE
        ),
        vec![Value::Text(array.clone())],
    )
    .await?;
    // 引用全没了（单被作废 / 删除）的行必须归零 —— 上一条 UPDATE 的 `FROM` 里没有它们
    ex.exec_sql(
        format!(
            "UPDATE {item_table} it \
                SET executed_qty = 0, executed_amount = 0, updated_at = now() \
              WHERE it.id = ANY(?::text::uuid[]) \
                AND NOT EXISTS ( \
                    SELECT 1 FROM biz_purchase_order_item i \
                      JOIN biz_purchase_order o ON o.id = i.order_id \
                     WHERE i.framework_agreement_item_id = it.id \
                       AND o.deleted_at IS NULL AND o.status <> 'CANCELLED')",
            item_table = fwa::ITEM_TABLE
        ),
        vec![Value::Text(array)],
    )
    .await?;
    // 单头合计（派生列）
    let rows = ex
        .query_sql(
            format!(
                "SELECT DISTINCT agreement_id FROM {item_table} WHERE id = ANY(?::text::uuid[])",
                item_table = fwa::ITEM_TABLE
            ),
            vec![Value::Text(format!(
                "{{{}}}",
                ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(",")
            ))],
        )
        .await?;
    for row in rows.iter() {
        let agreement_id: Uuid = row.get_typed("agreement_id")?;
        fwa::recalc_totals(ex, agreement_id).await?;
    }
    Ok(())
}

/// 一张采购单「不再占用额度」之后的收口（作废 / 删除都走这里）。
///
/// 只做一件事：把这张单引用过的协议行全部重算一遍 —— 单已被作废 / 软删，
/// 现算的累计量自然回落，额度就释放了（不需要「减回去」这种会漂移的手工账）。
pub async fn release_order(ex: &impl SqlExecutor, order_id: Uuid) -> DbResult<()> {
    let items = referenced_items_of_order(ex, order_id).await?;
    refresh_executed(ex, &items).await
}

/// 协议状态的中文名（报文里用；只覆盖协议域的三个状态）。
pub fn status_label(status: &str) -> &str {
    match status {
        fwa::STATUS_DRAFT => "拟稿",
        fwa::STATUS_SUBMITTED => "审批中",
        fwa::STATUS_APPROVED => "生效",
        _ => status,
    }
}

// ==================== 单测（纯逻辑 + 真库） ====================
//
// 纯逻辑（比价口径 / 上限口径）在本文件；真库链路（建单挂协议 / 超限拒绝 / 回写与释放）
// 在 `framework_agreement_ref_db_tests.rs`（同一串行锁，`ERP_DB_URL` 开关）。
#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// 不含税协议：协议价原样就是比价基准。
    #[test]
    fn exclusive_price_is_the_baseline() {
        assert_eq!(ex_tax_price(dec("10.000000"), false, dec("0.13")), dec("10.000000"));
        assert_eq!(
            price_cap(dec("10.000000"), false, dec("0.13"), dec("0")),
            dec("10.000000")
        );
        assert_eq!(
            price_cap(dec("10.000000"), false, dec("0.13"), dec("0.05")),
            dec("10.500000")
        );
    }

    /// 含税协议：先用**协议行的**税率（0–1 分数）倒推不含税，再套容差。
    ///
    /// 11.30 含税 13% → 10.000000 不含税；容差 0 时上限就是 10.00，
    /// 采购单行价 10.00 放行、10.01 拒绝（采购单行价恒为不含税）。
    #[test]
    fn tax_inclusive_price_is_normalised_before_comparison() {
        let excl = ex_tax_price(dec("11.30"), true, dec("0.13"));
        assert_eq!(excl, dec("10.000000"));
        assert_eq!(price_cap(dec("11.30"), true, dec("0.13"), dec("0")), dec("10.000000"));
        assert!(dec("10.000000") <= dec("10.000000"));
        assert!(dec("10.010000") > dec("10.000000"));
        // 6% 税率下同样的含税价折出来的不含税价更高（税率不同，基准就不同）
        assert_eq!(ex_tax_price(dec("11.30"), true, dec("0.06")), dec("10.660377"));
    }

    /// 6 位四舍五入（不是银行家舍入）：0.0000005 进位，避免与 purchase_model 差末位。
    #[test]
    fn price_normalisation_rounds_half_up_at_six_decimals() {
        assert_eq!(ex_tax_price(dec("1.000001"), false, dec("0.13")), dec("1.000001"));
        // 1 / 3 = 0.333333333… → 0.333333
        assert_eq!(ex_tax_price(dec("1.000000"), true, dec("2")), dec("0.333333"));
        // 2/3 = 0.666666666… → 0.666667
        assert_eq!(ex_tax_price(dec("2.000000"), true, dec("2")), dec("0.666667"));
    }

    /// 金额上限的比较口径跟协议价走：含税协议比价税合计，不含税协议比不含税金额。
    #[test]
    fn amount_cap_uses_the_agreement_price_basis() {
        assert_eq!(amount_for_cap(dec("100"), dec("113"), false), dec("100"));
        assert_eq!(amount_for_cap(dec("100"), dec("113"), true), dec("113"));
    }

    /// 容差 0 时「等于协议价」必须放行（`<=` 语义，不是 `<`）。
    #[test]
    fn zero_tolerance_still_allows_exact_agreed_price() {
        let cap = price_cap(dec("12.345678"), false, dec("0.13"), dec("0"));
        assert_eq!(cap, dec("12.345678"));
        assert!(dec("12.345678") <= cap, "等于上限必须放行（否则协议价自己都下不了单）");
        assert!(dec("12.345679") > cap);
    }
}
