// ============================================================================
// receivable_bridge.rs —— 销售出库过账 → 自动挂应收（SO-14）
//
// # 一个事务里做完五件事（顺序由 `stockout_handler::post_out_tx` 固定）
//
// ```text
// ① 锁单头 / 期间闸 / 制单≠过账 / 状态机 CAS           ← stockout_handler
// ② 三本账负向（成本账 / 库位账 / 流水）                ← stockout_model → inventory_writer
// ③ 单头金额 + 成本结转凭证（借 6401 / 1901，贷 1405）  ← voucher::create_for_stock_out
// ④ ★ 销售出库（source_type='SALES'）→ 生成应收单       ← 本文件
// ⑤ 跨单据联动（通用引擎，排除库存/凭证四类动作）       ← linkage::engine
// ```
//
// ④ 由 `post_out_tx` 直接调用，**没有任何自己的 commit / rollback**：应收挂不上、
// 客户查不到、金额算错 —— 任何一个 Err 都会让调用方把 ②③⑤ 一起回滚。这就是
// 「库存扣减 / 成本结转凭证 / 应收挂账同生共死」的落地方式（卡面硬要求）。
//
// # 为什么挂在 `post_out_tx`（出库过账内核）而不是 `shipment_posting`
//
// 销售出库单有两个入口都会过账：发货过账（`shipment_posting::post_in_tx` 在**自己的事务**里
// 调 `post_out_in_tx`）与手工建的销售来源出库单（`post_stock_out`）。挂在过账内核上，
// 两条路自动都有应收；挂在发货那条路上，手工路要再抄一遍判断。
//
// 判据是**出库单的 `source_type`**（V0285）而不是 `out_type`：
// `out_type` 只有 ISSUE/SCRAP/LOSS/OTHER（领用性质），销售出库的 out_type 是 OTHER。
//
// # 金额口径：出库金额**含税**
//
// ```text
// 一行发货明细 → 订单行单价 price / 税率 tax_rate
// 行含税 = round(price × 发货数量, 4) + round(不含税金额 × tax_rate / 100, 4)
// 应收金额 = Σ 行含税，再 round 到 4 位
// ```
//
// 金额**不取** `biz_stock_out.total_amount`：那是**成本**（借 6401），不是销售收入。
// 也不按订单行金额比例摊：直接复用 `sales_order_model::line_amounts` 现算 ——
// 与订单落库用的同一个函数，全量发货时 Σ 应收 == 订单含税总额（分文不差）。
//
// # 到期日 / 期间
//
// ```text
// period_code = voucher::period_of(out_date)          过账日所属会计期间（与成本凭证同期间）
// due_date    = out_date + biz_customer.credit_days    客户账期天数（V0289 新增，0 = 现结）
// ```
//
// # 幂等（三道闸）
//
// 1. `receivable::find_by_source(ex, "SALES_OUT", stock_out_id)` 命中即返回已有行，不重复挂；
// 2. 库层 `uk_biz_ar_source (source_type, source_id)`（部分唯一索引，MANUAL 例外）；
// 3. 撞唯一索引（并发两路过账）→ 按「已挂上」处理，回读那一行返回，不 500。
//
// # 客户 FROZEN 不拦（有意为之，别当漏了）
//
// 冻结的语义是「禁开**新**单」（`biz_customer.status` 注释、`create_manual` 的 409 闸），
// 而过账挂账是**已经审批通过的订单/发货单**的会计后果。过账这一刻因为客户被冻结而失败，
// 只会让货已经出了、账却挂不上的单据卡死。要拦冻结请拦在**建单**入口（SO-09 的建单守卫）。
//
// # 零元 / 负金额
//
// 含税金额 ≤ 0（赠品行、纯 0 价订单）**不建应收**，只记一条 warn：
// `biz_receivable.amount > 0` 是库层 CHECK，挂一张 0 元应收既过不了库层也没有业务含义。
// 这是「跳过」而不是「报错」—— 不能因为一张赠品行把整笔过账打回去。
//
// # 出库作废 → 冲销应收（V0290 / `cancel_in_tx`）
//
// 作废是过账的**逆动作**，顺序与过账对称：
//
// ```text
// 回滚三本账 + 冲销流水  →  出库红字凭证  →  ★ 应收作废（OPEN → CANCELLED）
// ```
//
// 判据与过账侧完全相同（同一张出库单、`source_type` 必须是 `SALES`、按
// `find_by_source('SALES_OUT', stock_out_id)` 找那张应收），所以「过账挂了应收」与
// 「作废冲销那笔应收」永远指向同一行 —— 不会出现「挂了却没冲」或「冲了别的单」。
//
// 口径（为什么不是 `WRITTEN_OFF` / 不是软删 / 不做负金额红字，见 V0290 与
// `receivable::cancel_for_stock_out` 的文档）：**只允许一分没收的 OPEN 被作废**；
// `PARTIAL` / `SETTLED` / `WRITTEN_OFF` 一律 409，整笔出库作废回滚
// （钱已经收了，要冲回必须走收款退款单 —— 那条路属后续卡，不在这里硬造）。
//
// 幂等：重复作废被出库单自己的状态机 CAS 挡在 409，根本进不到这里；
// 万一进到了（同一事务里被重复调用），已是 `CANCELLED` 的应收按「已冲销」返回，不再写第二次。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::receivable::{self, NewReceivable};
use crate::business::erp::sales_order_model;
use crate::business::erp::voucher;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 写进 `biz_receivable.source_type` 的来源类型（表注释口径：`SALES_OUT` 销售出库过账）。
///
/// **不要**用 `MANUAL`：`uk_biz_ar_source … WHERE source_type <> 'MANUAL'` 对 MANUAL 例外，
/// 用了就失去「同一出库单只挂一张应收」的幂等硬保证。
pub const SOURCE_TYPE_SALES_OUT: &str = "SALES_OUT";

/// `biz_stock_out.source_type` 的销售来源值（V0285，与 `stockout_base::SOURCE_TYPE_SALES` 同值）。
///
/// 这里单独声明而不是 `use`：桥接层的判据是**库里的字符串**，跟着 `stockout_base` 改名
/// 会静默改变挂账行为；两边由单测 `sales_out_source_type_matches_stockout_const` 钉死。
pub const OUT_SOURCE_SALES: &str = "SALES";

/// 建单状态日志（`NULL → OPEN`）的说明：一眼能看出这笔应收是过账自动挂的。
pub const CREATED_REASON: &str = "销售出库过账自动挂账";

// ==================== 纯函数（含税金额） ====================

/// 一行发货明细的可挂账要素（数量 / 订单行单价 / 订单行税率百分数）。
#[derive(Debug, Clone, PartialEq)]
pub struct ShipLine {
    /// 本次发货数量（`biz_shipment_item.qty`）。
    pub qty: Decimal,
    /// 订单行单价（**不含税**，`biz_sales_order_item.price`）。
    pub price: Decimal,
    /// 税率百分数（0–100，`biz_sales_order_item.tax_rate`）。
    pub tax_rate: Decimal,
}

/// 单行含税金额 = 不含税金额 + 税额（与订单落库同一套 `line_amounts`）。
pub fn line_taxed_amount(qty: Decimal, price: Decimal, tax_rate: Decimal) -> Decimal {
    let (amount, tax) = sales_order_model::line_amounts(qty, price, tax_rate);
    amount + tax
}

/// 整单应收金额（含税）= Σ 每行含税，**逐行 4 位定标后求和**，最后再定标一次。
///
/// 逐行定标是必须的：整单一次乘再加会让多行尾差与订单总额对不上
/// （订单是逐行 `round(price × qty, 4)` 落库的）。
pub fn taxed_amount_of(lines: &[ShipLine]) -> Decimal {
    lines
        .iter()
        .map(|line| line_taxed_amount(line.qty, line.price, line.tax_rate))
        .fold(Decimal::ZERO, |acc, amount| acc + amount)
        .round_dp(4)
}

// ==================== 库内读取 ====================

/// 出库单里挂账需要的几列（`StockOutRow` 不带来源两列，本模块自己取，不动公共列清单）。
#[derive(Debug, Clone)]
struct OutSource {
    /// 出库单号（写 `biz_receivable.source_no`）。
    out_no: String,
    /// 出库日期（过账日，算期间与到期日）。
    out_date: NaiveDate,
    /// 来源类型（V0285；`None` = 手工建单）。
    source_type: Option<String>,
    /// 来源单据 id（`source_type = 'SALES'` 时是 `biz_shipment.id`）。
    source_id: Option<Uuid>,
}

/// 读一张出库单的来源三要素（**不加锁**：调用方已经在过账事务里持有单头行锁）。
async fn out_source_of(ex: &impl SqlExecutor, stock_out_id: Uuid) -> std::result::Result<Option<OutSource>, ApiError> {
    // SCOPE-EXEMPT: 出库单是 ERP 单据表（无归属列），过账链路内部读。
    let row = ex
        .query_first_sql(
            "SELECT out_no, out_date, source_type, source_id::text AS source_id \
               FROM biz_stock_out WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&stock_out_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询出库单来源"))?;
    let Some(row) = row.as_ref() else {
        return Ok(None);
    };
    let raw_source_id: Option<String> = row
        .get_typed("source_id")
        .map_err(|error| utils::db_error(error, "读取出库单来源 id"))?;
    let source_id = match raw_source_id.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(Uuid::parse_str(text).map_err(|_| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("出库单 {stock_out_id} 的来源 id 不是合法 UUID：{text}"),
            )
        })?),
        _ => None,
    };
    Ok(Some(OutSource {
        out_no: row
            .get_typed("out_no")
            .map_err(|error| utils::db_error(error, "读取出库单号"))?,
        out_date: row
            .get_typed("out_date")
            .map_err(|error| utils::db_error(error, "读取出库日期"))?,
        source_type: row
            .get_typed("source_type")
            .map_err(|error| utils::db_error(error, "读取出库来源类型"))?,
        source_id,
    }))
}

/// 读发货单的客户 + 逐行挂账要素（单价 / 税率来自订单行，数量来自发货行）。
async fn shipment_customer_and_lines(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
) -> std::result::Result<(Uuid, Vec<ShipLine>), ApiError> {
    // SCOPE-EXEMPT: 发货单是 ERP 单据表（无归属列），过账链路内部读。
    let shipment = ex
        .query_first_sql(
            "SELECT customer_id FROM biz_shipment WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&shipment_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询发货单"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("出库单的来源发货单 {shipment_id} 不存在或已删除：无法确定应收的客户与金额，过账已回滚"),
            )
        })?;
    let customer_id: Uuid = shipment
        .get_typed("customer_id")
        .map_err(|error| utils::db_error(error, "读取发货单客户"))?;

    // SCOPE-EXEMPT: 发货明细 / 订单行都是 ERP 单据明细（无归属列）。
    let rows = ex
        .query_sql(
            "SELECT it.qty, oi.price, oi.tax_rate \
               FROM biz_shipment_item it \
               JOIN biz_sales_order_item oi ON oi.id = it.so_item_id \
              WHERE it.shipment_id = ?::uuid \
              ORDER BY it.created_at, it.id"
                .to_string(),
            vec![Value::from(&shipment_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询发货明细与订单行单价"))?;
    let lines = rows
        .iter()
        .map(|row| {
            Ok(ShipLine {
                qty: row
                    .get_typed("qty")
                    .map_err(|error| utils::db_error(error, "读取发货数量"))?,
                price: row
                    .get_typed("price")
                    .map_err(|error| utils::db_error(error, "读取订单行单价"))?,
                tax_rate: row
                    .get_typed("tax_rate")
                    .map_err(|error| utils::db_error(error, "读取订单行税率"))?,
            })
        })
        .collect::<std::result::Result<Vec<_>, ApiError>>()?;
    Ok((customer_id, lines))
}

/// 来源销售订单的币种与已锁定汇率（SO-40）。
///
/// 取路径：`biz_shipment_item.so_item_id → biz_sales_order_item.so_id → biz_sales_order`。
/// 发货明细为空 / 订单行缺失时回落到账套本位币 + 汇率 1（CNY 单的历史行为），
/// 不因为「查不到币种」把整张出库过账打红 —— 但会打 warn 让运维看见。
async fn order_currency_of(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
) -> std::result::Result<(String, Decimal), ApiError> {
    // SCOPE-EXEMPT: 发货明细 / 销售订单都是 ERP 单据表（无归属列），过账链路内部读。
    let row = ex
        .query_first_sql(
            "SELECT o.currency_code, o.exchange_rate \
               FROM biz_shipment_item it \
               JOIN biz_sales_order_item oi ON oi.id = it.so_item_id \
               JOIN biz_sales_order o ON o.id = oi.so_id AND o.deleted_at IS NULL \
              WHERE it.shipment_id = ?::uuid \
              ORDER BY it.created_at, it.id LIMIT 1"
                .to_string(),
            vec![Value::from(&shipment_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询来源销售订单币种"))?;
    match row {
        Some(row) => Ok((
            row.get_typed::<String>("currency_code")
                .map_err(|error| utils::db_error(error, "读取来源订单币种"))?,
            row.get_typed::<Decimal>("exchange_rate")
                .map_err(|error| utils::db_error(error, "读取来源订单汇率"))?,
        )),
        None => {
            tracing::warn!(
                shipment_id = %shipment_id,
                "出库过账：找不到来源销售订单的币种，应收按本位币兜底"
            );
            Ok((
                crate::business::erp::currency::base_currency_code(ex)
                    .await
                    .map_err(|error| utils::db_error(error, "查账套本位币"))?,
                Decimal::ONE,
            ))
        }
    }
}

/// 客户账期天数（`biz_customer.credit_days`，V0289）；客户不存在 / 已软删 → 409。
async fn credit_days_of(ex: &impl SqlExecutor, customer_id: Uuid) -> std::result::Result<i32, ApiError> {
    // SCOPE-EXEMPT: 客户主数据（全局），过账链路内部读。
    let row = ex
        .query_first_sql(
            "SELECT credit_days FROM biz_customer WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&customer_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询客户账期"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("客户 {customer_id} 不存在或已删除：无法确定应收账期，过账已回滚"),
            )
        })?;
    row.get_typed("credit_days")
        .map_err(|error| utils::db_error(error, "读取客户账期天数"))
}

// ==================== 主入口 ====================

/// 一次挂账的结果（供调用方打日志 / 供单测断言）。
#[derive(Debug, Clone)]
pub struct BridgeOutcome {
    /// 应收单 id。
    pub receivable_id: Uuid,
    /// 应收单号（`AR-yyyyMMdd-000001`）。
    pub ar_no: String,
    /// 应收金额（含税）。
    pub amount: Decimal,
    /// 本次是否真的新插了一行（`false` = 幂等命中已有行）。
    pub created: bool,
}

/// 销售出库过账 → 挂应收。**必须在过账事务里调用**（`ex` = 那个事务）。
///
/// * 非销售来源（`source_type` 为空 / 不是 `SALES`）→ `Ok(None)`，账一分不动；
/// * 已经挂过（幂等闸命中）→ `Ok(Some(created = false))`；
/// * 含税金额 ≤ 0 → `Ok(None)` + warn（零元应收挂不上，见模块文档）；
/// * 其余错误（客户不存在 / 发货单不存在 / 库层约束）→ `Err`，让调用方整笔回滚。
pub async fn post_in_tx(
    ex: &impl SqlExecutor,
    stock_out_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<BridgeOutcome>, ApiError> {
    let Some(source) = out_source_of(ex, stock_out_id).await? else {
        return Ok(None);
    };
    if source.source_type.as_deref() != Some(OUT_SOURCE_SALES) {
        return Ok(None);
    }

    // 幂等闸①：同一出库单已有应收 → 直接返回，不重复挂（重试 / 并发都走这里）
    if let Some(existing) = receivable::find_by_source(ex, SOURCE_TYPE_SALES_OUT, stock_out_id)
        .await
        .map_err(|error| utils::db_error(error, "查已有应收（幂等闸）"))?
    {
        tracing::info!(
            stock_out_id = %stock_out_id,
            out_no = %source.out_no,
            ar_no = %existing.ar_no,
            "出库过账：该出库单已挂过应收，跳过（幂等）"
        );
        return Ok(Some(BridgeOutcome {
            receivable_id: existing.id,
            ar_no: existing.ar_no,
            amount: existing.amount,
            created: false,
        }));
    }

    let shipment_id = source.source_id.ok_or_else(|| {
        ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "出库单 {} 的来源类型是 SALES 但没有来源单据 id：数据不自洽，过账已回滚",
                source.out_no
            ),
        )
    })?;
    let (customer_id, lines) = shipment_customer_and_lines(ex, shipment_id).await?;
    let amount = taxed_amount_of(&lines);
    if amount <= Decimal::ZERO {
        tracing::warn!(
            stock_out_id = %stock_out_id,
            out_no = %source.out_no,
            shipment_id = %shipment_id,
            lines = lines.len(),
            "出库过账：含税金额 ≤ 0（赠品 / 零价），不生成应收（biz_receivable.amount > 0 是库层 CHECK）"
        );
        return Ok(None);
    }
    let credit_days = credit_days_of(ex, customer_id).await?;
    // SO-40：应收**继承来源销售订单的币种与已锁定汇率** —— 外币订单出库后挂出来的应收
    // 必须是同一币种、同一汇率（这里不重取 SPOT：订单上锁的就是这张单的最终汇率，
    // 出库过账晚几天发生不该让应收金额漂移）。
    let (currency_code, exchange_rate) = order_currency_of(ex, shipment_id).await?;
    let period_code = voucher::period_of(ex, source.out_date)
        .await
        .map_err(|error| utils::db_error(error, "解析过账日所属会计期间"))?;
    let due_date = receivable::due_date_of(source.out_date, credit_days);
    let remark = format!(
        "销售出库 {} 过账自动挂账（含税 {}；账期 {} 天）",
        source.out_no,
        amount.round_dp(2),
        credit_days
    );
    let new = NewReceivable {
        customer_id,
        source_type: SOURCE_TYPE_SALES_OUT,
        source_id: stock_out_id,
        source_no: &source.out_no,
        amount,
        received_amount: Decimal::ZERO,
        credit_days,
        due_date,
        period_code: &period_code,
        invoice_no: None,
        invoice_date: None,
        remark: Some(&remark),
        // 收入确认凭证（借 1122 / 贷 6001）属「收入凭证」那组卡：本卡不手搓分录，
        // 与 SO-13 手工挂账同口径留 None。
        voucher_id: None,
        currency_code: &currency_code,
        exchange_rate,
    };
    match receivable::insert_with_reason(ex, &new, operator, now, CREATED_REASON).await {
        Ok((receivable_id, ar_no)) => {
            tracing::info!(
                stock_out_id = %stock_out_id,
                out_no = %source.out_no,
                shipment_id = %shipment_id,
                ar_no = %ar_no,
                amount = %amount,
                period = %period_code,
                due_date = %due_date,
                credit_days,
                "出库过账：应收已自动挂账（与库存扣减 / 成本结转凭证同一事务）"
            );
            Ok(Some(BridgeOutcome {
                receivable_id,
                ar_no,
                amount,
                created: true,
            }))
        }
        // 幂等闸③：并发两路过账都过了闸①，靠库层唯一索引收口 —— 后来者读回先行者那行，
        // 不当成 500（账没有多挂一张，符合「重复过账幂等」）。
        Err(DbError::UniqueViolation { constraint, .. })
            if constraint
                .as_deref()
                .is_some_and(|name| name.contains("uk_biz_ar_source")) =>
        {
            let existing = receivable::find_by_source(ex, SOURCE_TYPE_SALES_OUT, stock_out_id)
                .await
                .map_err(|error| utils::db_error(error, "回读并发挂上的应收"))?
                .ok_or_else(|| {
                    ApiError::new(
                        409,
                        error_code::RESOURCE_CONFLICT,
                        format!(
                            "出库单 {} 的应收单已被并发过账挂上，但回读不到（请刷新后核对账龄）",
                            source.out_no
                        ),
                    )
                })?;
            tracing::info!(
                stock_out_id = %stock_out_id,
                ar_no = %existing.ar_no,
                "出库过账：并发挂账撞唯一索引，按已挂上处理（幂等）"
            );
            Ok(Some(BridgeOutcome {
                receivable_id: existing.id,
                ar_no: existing.ar_no,
                amount: existing.amount,
                created: false,
            }))
        }
        Err(error) => Err(utils::db_error(error, "出库过账挂应收")),
    }
}

/// 某张出库单挂出来的应收（`None` = 没挂过）。列表 / 详情回显与单测用。
pub async fn receivable_of(ex: &impl SqlExecutor, stock_out_id: Uuid) -> DbResult<Option<receivable::ReceivableRow>> {
    receivable::find_by_source(ex, SOURCE_TYPE_SALES_OUT, stock_out_id).await
}

/// 一次作废冲销的结果（供调用方打日志 / 供单测断言）。
#[derive(Debug, Clone)]
pub struct CancelOutcome {
    /// 应收单 id。
    pub receivable_id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// `true` = 本次真的把应收从 `OPEN` 改成了 `CANCELLED`；
    /// `false` = 之前就已经是 `CANCELLED`（重复调用，幂等命中）。
    pub cancelled: bool,
}

/// 销售出库**作废** → 冲销那张自动挂上的应收。**必须在作废事务里调用**（`ex` = 那个事务）。
///
/// * 非销售来源（`source_type` 为空 / 不是 `SALES`）→ `Ok(None)`，账一分不动；
/// * 这张出库单当时没挂应收（零元 / 赠品 / SO-14 之前过账的老单）→ `Ok(None)` + info；
/// * 应收已是 `CANCELLED` → `Ok(Some(cancelled = false))`（幂等，不重复写）；
/// * 应收已收款 / 已核销 / 已结清 → `Err(409)`，调用方整笔回滚（三本账、红字凭证都不落地）；
/// * 正常 → `Ok(Some(cancelled = true))`。
pub async fn cancel_in_tx(
    ex: &impl SqlExecutor,
    stock_out_id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<CancelOutcome>, ApiError> {
    let Some(source) = out_source_of(ex, stock_out_id).await? else {
        return Ok(None);
    };
    if source.source_type.as_deref() != Some(OUT_SOURCE_SALES) {
        return Ok(None);
    }
    let Some(existing) = receivable::find_by_source(ex, SOURCE_TYPE_SALES_OUT, stock_out_id)
        .await
        .map_err(|error| utils::db_error(error, "查这张出库单的应收"))?
    else {
        // 过账时含税金额 ≤ 0 或者 SO-14 之前就过账了：本来就没有应收，作废也不用冲。
        tracing::info!(
            stock_out_id = %stock_out_id,
            out_no = %source.out_no,
            "出库作废：这张单没有挂过应收（零元 / 老单），无需冲销"
        );
        return Ok(None);
    };
    if existing.status == receivable::STATUS_CANCELLED {
        tracing::info!(
            stock_out_id = %stock_out_id,
            out_no = %source.out_no,
            ar_no = %existing.ar_no,
            "出库作废：应收已是已作废（幂等命中，不再重复冲销）"
        );
        return Ok(Some(CancelOutcome {
            receivable_id: existing.id,
            ar_no: existing.ar_no,
            cancelled: false,
        }));
    }

    let cancelled = receivable::cancel_for_stock_out(ex, existing.id, reason, operator, now).await?;
    tracing::info!(
        stock_out_id = %stock_out_id,
        out_no = %source.out_no,
        ar_no = %cancelled.ar_no,
        amount = %cancelled.amount,
        reason,
        "出库作废：应收已冲销（OPEN → CANCELLED，与三本账回滚 / 红字凭证同一事务）"
    );
    Ok(Some(CancelOutcome {
        receivable_id: cancelled.id,
        ar_no: cancelled.ar_no,
        cancelled: true,
    }))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::receivable::STATUS_OPEN;
    use crate::business::erp::shipment_handler::{self, ShipmentStatusReq};
    use crate::business::erp::stockin_model;
    use crate::business::erp::stockout_base::{OUT_TYPE_OTHER, SOURCE_TYPE_SALES};
    use crate::business::shared::state_machine;
    use nik::prelude::chrono::Duration;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量非法")
    }

    // ---------- 纯逻辑（不连库） ----------

    /// 桥接层的来源类型常量必须与出库侧逐字一致（改一边不改另一边 = 静默挂不出应收）。
    #[test]
    fn sales_out_source_type_matches_stockout_const() {
        assert_eq!(OUT_SOURCE_SALES, SOURCE_TYPE_SALES);
        assert_eq!(OUT_SOURCE_SALES, "SALES");
        assert_eq!(SOURCE_TYPE_SALES_OUT, "SALES_OUT");
        // 销售出库的 out_type 是 OTHER（领用性质），来源与领用性质是两个维度
        assert_eq!(OUT_TYPE_OTHER, "OTHER");
        assert_ne!(SOURCE_TYPE_SALES_OUT, receivable::SOURCE_MANUAL);
    }

    /// 含税金额 = 不含税 + 税额；税率为 0 时就是 `price × qty`。
    #[test]
    fn line_taxed_amount_adds_tax() {
        assert_eq!(line_taxed_amount(dec("10"), dec("100"), dec("13")), dec("1130"));
        assert_eq!(line_taxed_amount(dec("1"), dec("99.99"), dec("13")), dec("112.9887"));
        assert_eq!(line_taxed_amount(dec("10"), dec("100"), Decimal::ZERO), dec("1000"));
        // 小数数量：3 × 1.5 = 4.5，税额 4.5 × 13% = 0.585
        assert_eq!(line_taxed_amount(dec("1.5"), dec("3"), dec("13")), dec("5.085"));
    }

    /// 整单含税 = 逐行定标后求和（不是「先求和再乘税率」）。
    #[test]
    fn taxed_amount_of_sums_rounded_lines() {
        let lines = vec![
            ShipLine {
                qty: dec("10"),
                price: dec("100"),
                tax_rate: dec("13"),
            },
            ShipLine {
                qty: dec("3"),
                price: dec("33.3333"),
                tax_rate: dec("6"),
            },
        ];
        // 行1 = 1000 + 130 = 1130；行2 = 33.3333×3 = 99.9999，税额 99.9999×6% = 5.999994 → 6.0000
        assert_eq!(taxed_amount_of(&lines), dec("1235.9999"));
        assert_eq!(taxed_amount_of(&[]), Decimal::ZERO);
    }

    /// 全量发货时 Σ 应收 == 订单行含税总额（与订单落库同一套 `line_amounts`，分文不差）。
    #[test]
    fn full_shipment_total_equals_order_line_total() {
        for (qty, price, rate) in [
            ("10", "100", "13"),
            ("3", "33.3333", "6"),
            ("7", "0.005", "9"),
            ("1", "1234.5678", "13"),
        ] {
            let (amount, tax) = sales_order_model::line_amounts(dec(qty), dec(price), dec(rate));
            let shipped = taxed_amount_of(&[ShipLine {
                qty: dec(qty),
                price: dec(price),
                tax_rate: dec(rate),
            }]);
            assert_eq!(
                shipped,
                amount + tax,
                "数量 {qty} 单价 {price} 税率 {rate}：应收 {shipped} 应等于订单行含税 {}",
                amount + tax
            );
        }
    }

    /// 部分发货按**实发数量**算，不是按订单行金额比例摊。
    #[test]
    fn partial_shipment_uses_shipped_qty() {
        let full = taxed_amount_of(&[ShipLine {
            qty: dec("10"),
            price: dec("100"),
            tax_rate: dec("13"),
        }]);
        let part = taxed_amount_of(&[ShipLine {
            qty: dec("3"),
            price: dec("100"),
            tax_rate: dec("13"),
        }]);
        assert_eq!(full, dec("1130"));
        assert_eq!(part, dec("339"));
        assert!(part < full);
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed，属「假绿」。
    //   交付前必须显式带库跑一遍（卡内汇报记了实测命令）。
    //
    // 覆盖卡面三项验收：正常挂账 / 事务回滚一致 / 重复过账幂等。

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SO14T-` 夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const WAREHOUSE: &str = "d510f000-0000-4000-8000-000000000002";
    const MAT_A: &str = "d510f000-0000-4000-8000-000000000011";
    const CUSTOMER_CODE: &str = "SO14T-CUST";
    const MATS: &str = "'d510f000-0000-4000-8000-000000000011'::uuid, \
                        'd510f000-0000-4000-8000-000000000012'::uuid";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
         VALUES ('d510f000-0000-4000-8000-000000000001', 'SO14-PCS', '个', 1, NULL, 1.000000, 1, 1) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
         VALUES ('d510f000-0000-4000-8000-000000000002', 'SO14-WH', 'SO-14 挂账仓', \
                 '/d510f000-0000-4000-8000-000000000002/', 1, 1, 1, 0) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
         VALUES ('d510f000-0000-4000-8000-000000000011', 'SO14-M1', 'SO-14 物料甲', \
                 'd510f000-0000-4000-8000-000000000001', 1, false), \
                ('d510f000-0000-4000-8000-000000000012', 'SO14-M2', 'SO-14 物料乙', \
                 'd510f000-0000-4000-8000-000000000001', 1, false) \
         ON CONFLICT DO NOTHING",
    ];

    fn uuid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 UUID 非法")
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn sql(statement: &str) {
        db().exec_sql(statement.to_string(), vec![])
            .await
            .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
    }

    /// 清场：只清本模块夹具（`SO14T-` 客户 / 两张夹具物料），不碰别的用例。
    async fn cleanup() {
        let customers = "(SELECT id FROM biz_customer WHERE code LIKE 'SO14T-%')".to_string();
        let orders = format!("(SELECT id FROM biz_sales_order WHERE customer_id IN {customers})");
        let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN {orders})");
        let stock_outs = format!("(SELECT id FROM biz_stock_out WHERE source_id IN {shipments})");
        // ⚠ 顺序：状态日志要在删应收 / 出库单**之前**清（entity_id 子查询依赖它们还在）
        sql(&format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'RECEIVABLE' AND entity_id IN \
                    (SELECT id FROM biz_receivable WHERE source_id IN {stock_outs})) \
             OR (entity_type = 'STOCK_OUT' AND entity_id IN {stock_outs}) \
             OR (entity_type = 'SHIPMENT' AND entity_id IN {shipments}) \
             OR (entity_type = 'SALE_ORDER' AND entity_id IN {orders})"
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id IN {stock_outs}"
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs}"
        ))
        .await;
        sql(&format!("DELETE FROM biz_stock_out WHERE source_id IN {shipments}")).await;
        sql(&format!("DELETE FROM biz_shipment WHERE so_id IN {orders}")).await;
        sql(&format!("DELETE FROM biz_sales_order WHERE customer_id IN {customers}")).await;
        sql("DELETE FROM biz_customer WHERE code LIKE 'SO14T-%'").await;
        sql(&format!("DELETE FROM biz_inventory_txn WHERE material_id IN ({MATS})")).await;
        sql(&format!("DELETE FROM biz_inventory WHERE material_id IN ({MATS})")).await;
        sql(&format!(
            "DELETE FROM biz_inventory_location WHERE material_id IN ({MATS})"
        ))
        .await;
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        for statement in FIXTURES {
            sql(statement).await;
        }
        Some(guard)
    }

    /// 造库存（成本账 + 库位账，走入库侧同一批写入口）。
    async fn seed_stock(material: &str, qty: &str) {
        let now = Utc::now();
        let material_id = uuid(material);
        let warehouse_id = uuid(WAREHOUSE);
        let quantity = dec(qty);
        let amount = (quantity * dec("10")).round_dp(4);
        stockin_model::apply_location_qty(db(), material_id, warehouse_id, None, quantity, now)
            .await
            .expect("库位账加法（未分配桶）");
        stockin_model::apply_cost(
            db(),
            material_id,
            warehouse_id,
            None,
            stockin_model::CostBasis::of(quantity, amount),
            now,
        )
        .await
        .expect("成本账加法");
    }

    async fn cost_qty(material: &str) -> Decimal {
        let text: Option<String> = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(material))],
            )
            .await
            .expect("读成本账");
        text.and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    }

    /// 夹具客户（可指定账期天数；`code` 可换，用来造「客户不存在」场景）。
    async fn fixture_customer(code: &str, credit_days: i32) -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'SO-14 夹具客户', ?, 100000, 'ACTIVE')",
            vec![Value::from(code), Value::from(credit_days)],
        )
        .await
        .expect("插夹具客户");
        db().scalar_with_params::<Uuid>(
            "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
            vec![Value::from(code)],
        )
        .await
        .expect("查夹具客户")
    }

    /// 一张审批通过的订单 + 一行明细（`amount` / `tax_amount` 按 `line_amounts` 现算落库）。
    async fn fixture_order(customer: Uuid, qty: &str, price: &str, tax_rate: &str, status: &str) -> (Uuid, Uuid) {
        let so_id = Uuid::now_v7();
        let so_no = format!("SO14T-SO-{}", &so_id.to_string()[24..]);
        let (amount, tax_amount) = sales_order_model::line_amounts(dec(qty), dec(price), dec(tax_rate));
        db().execute_with_params(
            "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
             VALUES (?, ?, ?, CURRENT_DATE, ?)",
            vec![
                Value::from(so_id),
                Value::from(so_no),
                Value::from(customer),
                Value::from(status),
            ],
        )
        .await
        .expect("插夹具订单");
        let item_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item \
                (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
             VALUES (?, ?, 1, ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, 0)",
            vec![
                Value::from(item_id),
                Value::from(so_id),
                Value::from(&uuid(MAT_A)),
                Value::Text(qty.to_string()),
                Value::Text(price.to_string()),
                Value::Text(amount.to_string()),
                Value::Text(tax_rate.to_string()),
                Value::Text(tax_amount.to_string()),
            ],
        )
        .await
        .expect("插夹具订单行");
        (so_id, item_id)
    }

    /// 直接落一张 `PENDING_POST` 的发货单（绕过建单守卫；本卡验的是过账挂账）。
    async fn fixture_shipment(customer: Uuid, so_id: Uuid, so_item_id: Uuid, qty: &str, ship_date_offset: i64) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_shipment \
                (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
             VALUES (?, ?, ?, ?, ?, CURRENT_DATE + ?::int, 'PENDING_POST', ?::text::numeric)",
            vec![
                Value::from(id),
                Value::from(format!("SO14T-SH-{}", &id.to_string()[24..])),
                Value::from(so_id),
                Value::from(customer),
                Value::from(&uuid(WAREHOUSE)),
                Value::from(i32::try_from(ship_date_offset).expect("偏移量在 i32 内")),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("插夹具发货单");
        db().execute_with_params(
            "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
             VALUES (?, ?, ?, ?::text::numeric)",
            vec![
                Value::from(id),
                Value::from(so_item_id),
                Value::from(&uuid(MAT_A)),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("插夹具发货明细");
        id
    }

    /// 过账（走 handler 公开入口：状态机 + 事务 + 库存 + 凭证 + 挂账一条龙）。
    async fn post_shipment(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
        shipment_handler::change_shipment_status(
            None,
            &ship_id.to_string(),
            ShipmentStatusReq {
                target_status: "POSTED".to_string(),
                reason: None,
            },
        )
        .await
    }

    async fn text_of(query: &str, params: Vec<Value>) -> Option<String> {
        match db().scalar_with_params::<String>(query, params).await {
            Ok(text) => Some(text),
            Err(DbError::NotFound) => None,
            Err(error) => panic!("标量查询失败：{error}\n{query}"),
        }
    }

    /// 出库单 id（按来源发货单）。
    async fn stock_out_of(ship_id: Uuid) -> Option<Uuid> {
        db().scalar_with_params::<Uuid>(
            "SELECT id FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&ship_id)],
        )
        .await
        .ok()
    }

    async fn receivable_count(ship_id: Uuid) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_receivable r \
               JOIN biz_stock_out o ON o.id = r.source_id \
              WHERE o.source_id = ?::uuid AND r.source_type = 'SALES_OUT' \
                AND r.deleted_at IS NULL",
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("数应收行数")
    }

    /// **正常挂账**：发货过账 → 应收自动生成，金额 = 出库金额含税，来源 / 期间 / 到期日全对。
    #[tokio::test]
    async fn post_shipment_creates_receivable_in_same_transaction() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, 30).await;
        seed_stock(MAT_A, "20").await;
        // 10 × 100 含 13% 税 → 含税 1130
        let (so_id, item_id) = fixture_order(customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "10", 0).await;

        let res = post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("过账应成功：{} {}", error.code, error.msg));
        assert_eq!(res.status, "POSTED");

        let out_id = stock_out_of(ship_id).await.expect("过账必须生成销售出库单");
        let row = receivable_of(db(), out_id)
            .await
            .expect("查应收")
            .expect("销售出库过账必须自动挂应收");
        assert_eq!(row.source_type, SOURCE_TYPE_SALES_OUT);
        assert_eq!(row.source_id, out_id);
        assert_eq!(row.customer_id, customer);
        assert_eq!(row.status, STATUS_OPEN, "新挂的应收必须是 OPEN");
        assert_eq!(row.amount, dec("1130"), "应收金额 = 出库金额含税");
        assert_eq!(row.received_amount, Decimal::ZERO);
        assert_eq!(row.unsettled_amount, dec("1130"), "未收 = 应收 - 已收");
        assert_eq!(row.credit_days, 30);
        let out_no = text_of(
            "SELECT out_no FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&out_id)],
        )
        .await
        .expect("出库单号");
        assert_eq!(row.source_no, out_no, "source_no 必须等于出库单号");
        // 到期日 = 出库日 + 账期；期间 = 出库日所属期间（与成本凭证同期间）
        let out_date = text_of(
            "SELECT out_date::text FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&out_id)],
        )
        .await
        .expect("出库日期");
        let out_date = NaiveDate::from_str(&out_date).expect("日期");
        assert_eq!(row.due_date, out_date + Duration::days(30));
        assert_eq!(
            row.period_code,
            voucher::period_of(db(), out_date).await.expect("期间"),
            "period_code 必须取过账日所属期间"
        );
        // 建单日志的来源说明要能区分「自动挂账」与「手工挂账」
        let reason = text_of(
            "SELECT reason FROM biz_status_log \
              WHERE entity_type = 'RECEIVABLE' AND entity_id = ?::uuid ORDER BY created_at LIMIT 1",
            vec![Value::from(&row.id)],
        )
        .await;
        assert_eq!(reason.as_deref(), Some(CREATED_REASON));
        println!(
            "[SO-14] 正常挂账：出库单 {out_no} → 应收 {} 金额 {} 到期 {} 期间 {}",
            row.ar_no, row.amount, row.due_date, row.period_code
        );
    }

    /// **账期 0 = 现结**：到期日 = 过账日（不是硬编码 30 天）。
    #[tokio::test]
    async fn zero_credit_days_means_due_on_posting_date() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer("SO14T-CUST0", 0).await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "5", "10", "0", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "5", 0).await;
        post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("过账应成功：{} {}", error.code, error.msg));

        let out_id = stock_out_of(ship_id).await.expect("出库单");
        let row = receivable_of(db(), out_id).await.expect("查应收").expect("必须挂应收");
        let out_date = text_of(
            "SELECT out_date::text FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&out_id)],
        )
        .await
        .expect("出库日期");
        assert_eq!(row.credit_days, 0);
        assert_eq!(
            row.due_date,
            NaiveDate::from_str(&out_date).expect("日期"),
            "账期 0 = 现结，到期日就是过账日"
        );
        assert_eq!(row.amount, dec("50"), "5 × 10，税率 0");
    }

    /// **事务回滚一致**：应收挂不上（客户不存在）→ 库存 / 出库单 / 应收一分不动。
    ///
    /// 这条用例是卡面「必须与库存扣减、成本结转凭证在同一事务」的**反向证明**：
    /// 挂账发生在三本账已经减完之后（见 `post_out_tx` 的 ③→④ 顺序），
    /// 如果口径不在同一事务，这里会留下「库存减了、应收没挂」的半截账。
    #[tokio::test]
    async fn missing_customer_rolls_back_stock_and_receivable() {
        let Some(_guard) = setup().await else { return };
        seed_stock(MAT_A, "20").await;
        // 客户压根不存在（`biz_shipment.customer_id` 是弱引用，没有外键挡住）
        let ghost_customer = Uuid::now_v7();
        let (so_id, item_id) = fixture_order(ghost_customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(ghost_customer, so_id, item_id, "10", 0).await;

        let error = post_shipment(ship_id)
            .await
            .expect_err("客户不存在，过账必须失败（不能挂一张没有客户的应收）");
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT, "应是 409 冲突");
        assert!(error.msg.contains("客户"), "错误文案要点名客户，实际 {}", error.msg);

        assert_eq!(cost_qty(MAT_A).await, dec("20"), "库存必须原封不动");
        assert_eq!(receivable_count(ship_id).await, 0, "不许留下半截应收");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_shipment WHERE id = ?::uuid",
                vec![Value::from(&ship_id)]
            )
            .await
            .as_deref(),
            Some("PENDING_POST"),
            "发货单状态不许被推进"
        );
        assert!(
            stock_out_of(ship_id).await.is_none(),
            "出库单必须随事务一起回滚（不留孤儿）"
        );
        println!("[SO-14] 回滚一致：过账失败后 库存 20 / 应收 0 / 发货单仍 PENDING_POST");
    }

    /// **重复过账幂等**：第二次过账 409，且应收只有一张。
    #[tokio::test]
    async fn repeated_post_keeps_single_receivable() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, 15).await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "10", 0).await;

        post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("第一次过账应成功：{} {}", error.code, error.msg));
        let first = receivable_count(ship_id).await;
        assert_eq!(first, 1, "第一次过账后应有 1 张应收");

        let error = post_shipment(ship_id).await.expect_err("重复过账必须被幂等闸挡住");
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
        assert_eq!(receivable_count(ship_id).await, 1, "重复过账不许挂第二张应收");
        assert_eq!(cost_qty(MAT_A).await, dec("10"), "库存只扣一次");

        // 直接从桥接层再调一次：幂等闸①命中已有行，返回 created=false
        let out_id = stock_out_of(ship_id).await.expect("出库单");
        let again = post_in_tx(db(), out_id, None, Utc::now())
            .await
            .expect("幂等重入不应报错")
            .expect("应返回已有应收");
        assert!(!again.created, "重入必须走幂等命中分支");
        assert_eq!(again.amount, dec("1130"));
        assert_eq!(receivable_count(ship_id).await, 1);
        println!(
            "[SO-14] 幂等：重复过账 409，应收仍 1 张（{}），重入 created=false",
            again.ar_no
        );
    }

    /// 非销售来源（手工领用出库）→ **不挂**应收（判据是 source_type，不是 out_type）。
    #[tokio::test]
    async fn manual_stock_out_does_not_create_receivable() {
        let Some(_guard) = setup().await else { return };
        seed_stock(MAT_A, "20").await;
        let operator = state_machine::Operator::named(None, "SO14 单测");
        let now = Utc::now();
        let new = crate::business::erp::stockout_base::NewStockOut {
            out_type: OUT_TYPE_OTHER,
            warehouse_id: uuid(WAREHOUSE),
            out_date: crate::business::erp::voucher::today_cst(now),
            reason: Some("SO-14 单测：手工领用"),
            remark: None,
            source_type: None,
            source_id: None,
        };
        let lines = vec![crate::business::erp::stockout_base::NewStockOutLine {
            material_id: uuid(MAT_A),
            unit_id: uuid("d510f000-0000-4000-8000-000000000001"),
            qty: dec("2"),
            location_id: None,
            lot_no: None,
            remark: None,
        }];
        let totals = crate::business::erp::stockout_base::Totals::of(&lines);
        let out_id = crate::business::erp::stockout_model::create(db(), &new, &lines, &totals, operator.clone(), now)
            .await
            .expect("建手工出库单");
        crate::business::erp::stockout_handler::post_stock_out(out_id, operator, None)
            .await
            .unwrap_or_else(|error| panic!("手工出库过账应成功：{} {}", error.code, error.msg));

        assert!(
            receivable_of(db(), out_id).await.expect("查应收").is_none(),
            "source_type 为空的出库单不许挂应收"
        );
        assert_eq!(cost_qty(MAT_A).await, dec("18"), "手工出库照常扣库存");
    }

    // ---------- 出库作废 → 应收冲销（V0290 / `cancel_in_tx`） ----------

    /// 账龄明细里本单是否在册（走真正的账龄取数：`receivable_aging::load_totals_and_details`）。
    async fn in_aging(ar_no: &str) -> bool {
        use crate::business::erp::receivable_aging;
        let today = voucher::today_cst(Utc::now());
        let period = voucher::period_of(db(), today).await.expect("当前期间");
        let tx = db().begin().await.expect("开只读快照事务");
        let outcome = receivable_aging::load_totals_and_details(&tx, today, &period).await;
        let _ = tx.rollback().await;
        let (_, _, _, _, _, details) = outcome.expect("账龄取数");
        details.iter().any(|row| row.ar_no == ar_no)
    }

    /// 作废销售出库单 → 应收 `OPEN → CANCELLED`、账龄不再计提、三本账回滚（同一事务）。
    #[tokio::test]
    async fn cancel_stock_out_cancels_receivable_and_restores_stock() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, 30).await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "10", 0).await;
        post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("过账应成功：{} {}", error.code, error.msg));

        let out_id = stock_out_of(ship_id).await.expect("过账生成出库单");
        let before = receivable_of(db(), out_id)
            .await
            .expect("查应收")
            .expect("过账必须挂应收");
        assert_eq!(before.status, STATUS_OPEN);
        assert_eq!(before.amount, dec("1130"));
        assert!(in_aging(&before.ar_no).await, "未收款的应收必须进账龄");
        assert_eq!(cost_qty(MAT_A).await, dec("10"), "过账后库存 20 → 10");

        const REASON: &str = "客户取消订单，货退回仓库";
        crate::business::erp::stockout_handler::cancel_stock_out(out_id, REASON, None)
            .await
            .unwrap_or_else(|error| panic!("作废应成功：{} {}", error.code, error.msg));

        let after = receivable_of(db(), out_id)
            .await
            .expect("查应收")
            .expect("作废后应收行必须还在（终态留痕，不是软删）");
        assert_eq!(after.id, before.id, "冲销的必须是同一张应收");
        assert_eq!(
            after.status,
            receivable::STATUS_CANCELLED,
            "作废后应收状态必须是 CANCELLED（不是 WRITTEN_OFF 坏账）"
        );
        assert_eq!(after.received_amount, Decimal::ZERO, "一分没收");
        assert_eq!(after.unsettled_amount, dec("1130"), "金额不动，恒等式仍成立");
        assert!(
            after.settled_at.is_some(),
            "作废要落 settled_at（封账时间），台账才看得出什么时候终止的"
        );
        let remark = after.remark.clone().unwrap_or_default();
        assert!(
            remark.contains(receivable::CANCEL_REASON_PREFIX) && remark.contains(REASON),
            "备注要留冲销前缀与上游作废原因，实际 {remark}"
        );
        assert!(!remark.contains("坏账"), "作废不许留下坏账字样（坏账报表会串味）");

        // 状态日志：OPEN → CANCELLED + 原因（谁、为什么）
        let log = text_of(
            "SELECT from_status || '>' || to_status || '|' || coalesce(reason, '') \
               FROM biz_status_log \
              WHERE entity_type = 'RECEIVABLE' AND entity_id = ?::uuid AND to_status = 'CANCELLED' \
              ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(&after.id)],
        )
        .await
        .expect("作废必须写状态日志");
        assert_eq!(log, format!("OPEN>CANCELLED|{REASON}"));

        // 账龄不再计提 + 三本账回滚 + 单据终态
        assert!(!in_aging(&before.ar_no).await, "已作废的应收不许再进账龄");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "作废把库存原路退回");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_stock_out WHERE id = ?::uuid",
                vec![Value::from(&out_id)]
            )
            .await
            .as_deref(),
            Some("CANCELLED")
        );
        println!(
            "[SO-14 遗留] 作废冲销：{} → CANCELLED（金额 {}，账龄已排除，库存回滚到 20）",
            after.ar_no, after.amount
        );
    }

    /// **已收过款**的应收挡住作废：409 + 整笔回滚（出库单仍 POSTED、库存仍扣着、没有红字凭证）。
    #[tokio::test]
    async fn cancel_blocked_when_receivable_has_receipts() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, 30).await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "10", 0).await;
        post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("过账应成功：{} {}", error.code, error.msg));

        let out_id = stock_out_of(ship_id).await.expect("出库单");
        let ar = receivable_of(db(), out_id).await.expect("查应收").expect("应收");
        // 模拟已经收过 40 元（收款模块尚未落地，直接造 PARTIAL 形态）
        sql(&format!(
            "UPDATE biz_receivable SET received_amount = 40, unsettled_amount = 1090, \
                status = 'PARTIAL', updated_at = now() WHERE id = '{}'::uuid",
            ar.id
        ))
        .await;

        let error = crate::business::erp::stockout_handler::cancel_stock_out(out_id, "客户取消订单，货退回仓库", None)
            .await
            .expect_err("应收已收款，作废必须 409");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("已收") || error.msg.contains("收款"),
            "文案要点名已收款（先冲收款），实际 {}",
            error.msg
        );

        // 整笔回滚：单据仍 POSTED、库存仍 10、应收仍 PARTIAL、没有红字凭证
        assert_eq!(
            text_of(
                "SELECT status FROM biz_stock_out WHERE id = ?::uuid",
                vec![Value::from(&out_id)]
            )
            .await
            .as_deref(),
            Some("POSTED"),
            "作废失败后出库单必须留在 POSTED（不许半截作废）"
        );
        assert_eq!(cost_qty(MAT_A).await, dec("10"), "三本账必须原封不动");
        let still = receivable_of(db(), out_id).await.expect("查应收").expect("应收");
        assert_eq!(still.status, receivable::STATUS_PARTIAL);
        assert_eq!(still.received_amount, dec("40"));
        let red = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_voucher \
                  WHERE source_type = 'STOCK_OUT' AND source_id = ?::uuid AND is_red = 1",
                vec![Value::from(&out_id)],
            )
            .await
            .expect("数红字凭证");
        assert_eq!(red, 0, "作废失败不许留下红字凭证（同一事务）");
        println!(
            "[SO-14 遗留] 已收款拦住作废：{} / 出库单仍 POSTED / 红字凭证 0",
            error.msg
        );
    }

    /// 重复作废被出库单状态机挡在 409；应收侧重复调用走幂等分支，不再写第二次。
    #[tokio::test]
    async fn repeated_cancel_keeps_receivable_cancelled_once() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, 30).await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "100", "13", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, "10", 0).await;
        post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("过账应成功：{} {}", error.code, error.msg));
        let out_id = stock_out_of(ship_id).await.expect("出库单");

        const REASON: &str = "客户取消订单，货退回仓库";
        crate::business::erp::stockout_handler::cancel_stock_out(out_id, REASON, None)
            .await
            .expect("第一次作废");
        let second = crate::business::erp::stockout_handler::cancel_stock_out(out_id, REASON, None)
            .await
            .expect_err("重复作废必须 409");
        assert_eq!(second.status, 409);

        // 直接重入桥接层：已 CANCELLED → cancelled=false，不重复写
        let again = cancel_in_tx(db(), out_id, REASON, None, Utc::now())
            .await
            .expect("幂等重入不应报错")
            .expect("应返回那张已作废的应收");
        assert!(!again.cancelled, "第二次必须走幂等分支");

        let ar = receivable_of(db(), out_id).await.expect("查应收").expect("应收");
        assert_eq!(ar.status, receivable::STATUS_CANCELLED);
        let logs = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'RECEIVABLE' AND entity_id = ?::uuid AND to_status = 'CANCELLED'",
                vec![Value::from(&ar.id)],
            )
            .await
            .expect("数状态日志");
        assert_eq!(logs, 1, "只许留一条作废轨迹（幂等）");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "库存只回滚一次，不会被退两次");
        println!("[SO-14 遗留] 幂等：重复作废 409，应收 {} 只冲销一次", ar.ar_no);
    }
}
