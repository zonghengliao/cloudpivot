//! 客户信用额度：占用 / 释放 / 兜底重算（销售链 · SO-38 · ARCH-7 §15.5）。
//!
//! # v1.0 的漏洞（本卡要补的）
//!
//! v1.0 只写了「下单时校验 `credit_used + 本单金额 <= credit_limit`」，**没写什么时候释放**：
//! 只占用不释放 ⇒ 客户额度被永久占死。本文件补上三个释放出口 + 每日兜底重算。
//!
//! # 口径（唯一真相，改这里必须同步 `V0313` 头注释与 `erp_invariants.py` 的 S9）
//!
//! ```text
//! credit_used = SUM(有效订单含税) - SUM(已核销应收) - SUM(已过账退货含税)   下限 0
//!
//!   有效订单 = biz_sales_order.status IN (APPROVED, PARTIAL_OUT, OUTBOUND)
//!   已核销应收 = SUM(biz_receivable.received_amount)     （= 客户已经付过的钱）
//!   已过账退货 = SUM(biz_sales_return.total_with_tax) WHERE status = 'POSTED'
//!   容差 0.01（numeric(18,4) 存 4 位，口径比到分）
//! ```
//!
//! 为什么 `DRAFT` / `SUBMITTED` 不进「有效订单」：ARCH-7 §15.5.2「占用时机（唯一）：
//! 销售订单 APPROVED 时」。待审单不进计数器，但**提交守卫**仍算管道占用（[`pending_credit`]），
//! 否则「连开 10 张草稿一起提交」能绕过额度 —— 守卫比计数器严格，方向是安全的。
//!
//! 为什么 `OUTBOUND` 仍在集合里：货发了但钱没到，信用敞口一分没少（应收未收）。
//! 出集只有三条路：作废/关闭（订单不再有效）、收款核销、退货过账。
//!
//! # 三个释放出口（对齐卡面 ①②③）
//!
//! | 出口 | 触发点 | 本文件入口 |
//! |------|--------|------------|
//! | ① 订单作废 / 关闭且未发货 | `sales_order_model::apply_status` | [`sync_order`]（cause = `ORDER_RELEASE`） |
//! | ② 应收核销完成 | `receipt::…apply_allocations`（回写 received_amount） | [`sync`]（cause = `AR_SETTLED`） |
//! | ③ 退货过账 | `sales_return_posting::post`（冲减应收） | [`sync`]（cause = `RETURN_POSTED`） |
//!
//! 收款退款单（`receipt_refund::allocate`，received_amount 减少）同样触发 [`sync`]
//! （cause = `AR_REVERSED`）—— 钱退了，敞口又回来了，这条出口是「② 的反向边」。
//!
//! # 为什么「事件 = 重算」而不是「事件 = 计数器 += / -=」
//!
//! 卡面写的是 `credit_used += 金额` / `-= 金额`。实现上**故意做成「触发即重算」**：
//!
//! 1. **幂等**：同一次收款重放两遍、退货过账重试、审批回调重复投递，结果都一样。
//!    计数器写法在这三种情况下都会多减 / 多加，而这正是 v1.0 额度被占死的同类根因。
//! 2. **与 S9 同源**：S9 判据就是这条公式。事件时用同一个函数算，**不变量因此恒真**；
//!    计数器写法要靠「四个出口一个不漏」维持恒等，漏一处就漂移（本仓 R1 单一数据源，
//!    见 `V0215` 头注释与 `sales_order_model::customer_open_credit` 的同一段论述）。
//! 3. **每日重算仍然是必需的兜底**：库被手工 UPDATE、老数据回填、代码漏接出口，
//!    重算都能把坑填平并留痕（`biz_credit_log` cause = `RECOMPUTE`）。
//!
//! 重算的代价是每次多三条聚合查询（都命中 `customer_id` 索引），换来的是「账一定对」。
//!
//! # 已知边界（不是漏了）
//!
//! * `WRITTEN_OFF`（坏账）的应收：钱收不回来了，理论敞口应归零，但本公式只按
//!   `received_amount` 释放 ⇒ 该客户继续占着「核销损失」那部分额度（保守方向，不会超卖）。
//!   彻底解法要引入「坏账释放」第四个出口，属坏账卡的范围。
//! * `PARTIAL_OUT → CLOSED`（部分发货后关闭剩余量）：订单整张出集（ARCH §15.5.2 ① 的
//!   「关闭即全额释放」），已发货但未收款的那部分敞口此时不再被本计数器覆盖。
//!   这是 ARCH 公式本身的边界，S9 忠实照抄，不在本卡改口径。
//!
//! # 事务边界
//!
//! 所有写函数吃 `&impl SqlExecutor`，由调用方（订单状态流转 / 收款核销 / 退货过账）
//! **在同一个事务里**调用：额度与单据同生共死，不会出现「单子作废了额度没还」。
//!
//! ```text
//! SCOPE-EXEMPT: biz_customer / biz_credit_log 无归属列（客户与台账是全局主数据，
//! 数据范围 SELF / DEPT_* 表达不出来）；可见性由 business:erp:* 权限码控制。
//! ```

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 额度变动台账表（V0313）。
pub const LOG_TABLE: &str = "biz_credit_log";

/// 占信用额度的订单状态：**APPROVED 起占用**，发货（PARTIAL_OUT / OUTBOUND）不释放。
pub const OCCUPYING_STATUSES: [&str; 3] = [STATUS_APPROVED, STATUS_PARTIAL_OUT, STATUS_OUTBOUND];

/// 管道占用：已提交待审批（不进 `credit_used`，只在提交守卫里算）。
pub const PENDING_STATUSES: [&str; 1] = [STATUS_SUBMITTED];

/// 订单状态常量（与 `sales_model` 逐字一致；此处重列是为了本模块不依赖 handler 层）。
const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 见 [`STATUS_SUBMITTED`]。
const STATUS_APPROVED: &str = "APPROVED";
/// 见 [`STATUS_SUBMITTED`]。
const STATUS_PARTIAL_OUT: &str = "PARTIAL_OUT";
/// 见 [`STATUS_SUBMITTED`]。
const STATUS_OUTBOUND: &str = "OUTBOUND";

/// 变动原因：订单审批通过（占用）。
pub const CAUSE_ORDER_OCCUPY: &str = "ORDER_OCCUPY";
/// 变动原因：订单作废 / 关闭（释放出口 ①）。
pub const CAUSE_ORDER_RELEASE: &str = "ORDER_RELEASE";
/// 变动原因：订单其它状态变化（发货推进等，额度不变但记一笔便于对账）。
pub const CAUSE_ORDER_STATUS: &str = "ORDER_STATUS";
/// 变动原因：应收核销（释放出口 ②，主要出口）。
pub const CAUSE_AR_SETTLED: &str = "AR_SETTLED";
/// 变动原因：收款退款（② 的反向边，敞口回来）。
pub const CAUSE_AR_REVERSED: &str = "AR_REVERSED";
/// 变动原因：退货过账（释放出口 ③）。
pub const CAUSE_RETURN_POSTED: &str = "RETURN_POSTED";
/// 变动原因：每日兜底重算修正。
pub const CAUSE_RECOMPUTE: &str = "RECOMPUTE";

/// 定时任务名（同时是 `sys_job.invoke_target` 与调度器注册名，V0313 已种行）。
pub const TASK_NAME: &str = "erp_credit_recompute";

/// 兜底重算的 cron：每天 02:10。
///
/// 选在 02:20 的账务不变量巡检（`support::invariant::runner::CRON`）**之前**：
/// 先修正再巡检 —— 修过的库巡检自然是绿的；巡检报红就真的说明「今晚没修成」。
/// 重算自身的修正逐条打 WARN，不会静默。
pub const TASK_CRON: &str = "0 10 2 * * *";

/// 口径容差：0.01（分）。
pub fn tolerance() -> Decimal {
    Decimal::new(1, 2)
}

// ==================== 纯函数（可直接单测，不碰库） ====================

/// 按 §15.5.2 公式算「已用额度」：三项相减，**下限 0**。
///
/// 下限 0 的理由与 `sales_order_model::credited_after_returns` 相同：
/// 负占用会让下一张单白拿额度（历史单退货多于在建单时会出现这种算数结果）。
pub fn net_used(ordered: Decimal, received: Decimal, returned: Decimal) -> Decimal {
    let net = ordered - received - returned;
    if net < Decimal::ZERO {
        Decimal::ZERO
    } else {
        net.round_dp(2)
    }
}

/// 两个额度是否「在容差内相等」（比较到分；差正好 0.01 仍算相等）。
pub fn is_same(left: Decimal, right: Decimal) -> bool {
    let diff = (left - right).abs();
    diff <= tolerance()
}

/// 状态迁移对应的变动原因（订单侧）。
///
/// * 迁入占用集合 → [`CAUSE_ORDER_OCCUPY`]（唯一占用时机：APPROVED）；
/// * 迁出占用集合 → [`CAUSE_ORDER_RELEASE`]（作废 / 关闭）；
/// * 集合内互迁（APPROVED → PARTIAL_OUT → OUTBOUND）→ [`CAUSE_ORDER_STATUS`]，额度不变。
pub fn cause_of_transition(from: &str, to: &str) -> &'static str {
    let was = OCCUPYING_STATUSES.contains(&from);
    let now = OCCUPYING_STATUSES.contains(&to);
    match (was, now) {
        (false, true) => CAUSE_ORDER_OCCUPY,
        (true, false) => CAUSE_ORDER_RELEASE,
        _ => CAUSE_ORDER_STATUS,
    }
}

// ==================== 口径计算（读） ====================

/// 一次口径计算的三个分量 + 结果。
///
/// 分量单独留出来：S9 报红时要能一眼看出「是订单少了、还是收款没认」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditUsage {
    /// 有效订单含税合计。
    pub ordered: Decimal,
    /// 应收已核销（已收款）合计。
    pub received: Decimal,
    /// 已过账退货含税合计。
    pub returned: Decimal,
    /// 公式结果（下限 0，保留 2 位）。
    pub used: Decimal,
}

impl CreditUsage {
    /// 一行派生摘要，给日志 / 任务结果用。
    pub fn summary(&self) -> String {
        format!(
            "有效订单 {} − 已核销 {} − 已退货 {} = 已用 {}",
            self.ordered.round_dp(2),
            self.received.round_dp(2),
            self.returned.round_dp(2),
            self.used
        )
    }
}

/// 读一列 `::text` 聚合值并解析（`SUM` 的空集走 COALESCE，仍可能拿到空串，兜 0）。
fn sum_of(rows: &[Row], column: &str) -> DbResult<Decimal> {
    let text: Option<String> = rows.first().and_then(|row| row.get_typed(column).ok());
    Ok(text
        .as_deref()
        .and_then(|value| value.trim().parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

/// 按公式算某客户的额度占用（**不读缓存列**，三张表现算）。
///
/// 这是 S9 的同一个口径，也是所有「事件触发重算」的唯一实现。
pub async fn usage_of(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<CreditUsage> {
    let placeholders = vec!["?"; OCCUPYING_STATUSES.len()].join(", ");
    let mut params: Vec<Value> = vec![Value::from(&customer_id)];
    params.extend(
        OCCUPYING_STATUSES
            .iter()
            .map(|status| Value::Text((*status).to_string())),
    );
    // SCOPE-EXEMPT: 销售订单是 ERP 单据表，无归属列（见模块文档）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT COALESCE(SUM(total_with_tax), 0)::text AS ordered \
                   FROM biz_sales_order \
                  WHERE customer_id = ?::uuid AND deleted_at IS NULL \
                    AND status IN ({placeholders})"
            ),
            params,
        )
        .await?;
    let ordered = sum_of(&rows, "ordered")?;

    // SCOPE-EXEMPT: 应收台账无归属列。
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(received_amount), 0)::text AS received \
               FROM biz_receivable WHERE customer_id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&customer_id)],
        )
        .await?;
    let received = sum_of(&rows, "received")?;

    // SCOPE-EXEMPT: 退货单是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(total_with_tax), 0)::text AS returned \
               FROM biz_sales_return \
              WHERE customer_id = ?::uuid AND deleted_at IS NULL AND status = 'POSTED'"
                .to_string(),
            vec![Value::from(&customer_id)],
        )
        .await?;
    let returned = sum_of(&rows, "returned")?;

    Ok(CreditUsage {
        ordered,
        received,
        returned,
        used: net_used(ordered, received, returned),
    })
}

/// 读缓存列 `biz_customer.credit_used`（客户不存在 / 已软删 → `None`）。
pub async fn stored(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<Option<Decimal>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT credit_used::text AS used FROM {CUSTOMER_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&customer_id)],
        )
        .await?;
    let text: Option<String> = rows.first().and_then(|row| row.get_typed("used").ok());
    Ok(text
        .as_deref()
        .and_then(|value| value.trim().parse::<Decimal>().ok()))
}

/// 管道占用：已提交待审批订单的含税合计（提交守卫用，不进 `credit_used`）。
///
/// `exclude` = 编辑 / 提交时把「本单自己」排除（否则已提交的单会把自己算两遍）。
pub async fn pending_credit(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<Decimal> {
    let mut sql = format!(
        "SELECT COALESCE(SUM(total_with_tax), 0)::text AS pending FROM biz_sales_order \
          WHERE customer_id = ?::uuid AND deleted_at IS NULL AND status = ?"
    );
    let mut params = vec![
        Value::from(&customer_id),
        Value::Text(PENDING_STATUSES[0].to_string()),
    ];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let rows = ex.query_sql(sql, params).await?;
    sum_of(&rows, "pending")
}

// ==================== 写：占用 / 释放 / 重算 ====================

/// 一次同步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditSync {
    /// 客户。
    pub customer_id: Uuid,
    /// 同步前（缓存列）。
    pub before: Decimal,
    /// 同步后（公式结果）。
    pub after: Decimal,
    /// 是否真的写了库（容差内相等则不动，避免每次事件都产生一条无意义台账）。
    pub repaired: bool,
}

impl CreditSync {
    /// 变动额（正 = 占用增加，负 = 释放）。
    pub fn delta(&self) -> Decimal {
        (self.after - self.before).round_dp(2)
    }

    /// 一行摘要（日志 / 任务结果）。
    pub fn summary(&self) -> String {
        format!(
            "客户 {} 额度 {} → {}（Δ{}，{}）",
            self.customer_id,
            self.before.round_dp(2),
            self.after.round_dp(2),
            self.delta(),
            if self.repaired {
                "已修正"
            } else {
                "无变化"
            }
        )
    }
}

/// **把某客户的 `credit_used` 同步到公式结果**（占用 / 释放 / 重算共用这一个写入口）。
///
/// * `cause`：变动原因（[`CAUSE_ORDER_OCCUPY`] 等），落 `biz_credit_log`；
/// * `ref_no`：触发单据号（订单 / 应收 / 退货单号），便于对账时回查。
///
/// 容差内相等 → 不写库、不记台账（幂等：重放同一个事件不产生噪音）。
/// 客户不存在 / 已软删 → 原样返回 0，不报错（单据可能指向已删客户，那不该 500）。
pub async fn sync(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    cause: &str,
    ref_no: &str,
) -> DbResult<CreditSync> {
    // 并发重试：CAS 失配说明另一个会话刚改过同一客户（收款与退货并行），重读再算即可。
    for _ in 0..3 {
        let Some(before) = stored(ex, customer_id).await? else {
            return Ok(CreditSync {
                customer_id,
                before: Decimal::ZERO,
                after: Decimal::ZERO,
                repaired: false,
            });
        };
        let usage = usage_of(ex, customer_id).await?;
        let after = usage.used;
        if is_same(before, after) {
            return Ok(CreditSync {
                customer_id,
                before,
                after,
                repaired: false,
            });
        }
        let affected = ex
            .exec_sql(
                format!(
                    "UPDATE {CUSTOMER_TABLE} \
                        SET credit_used = NULLIF(?::text,'')::numeric, updated_at = ? \
                      WHERE id = ?::uuid AND deleted_at IS NULL \
                        AND credit_used = NULLIF(?::text,'')::numeric"
                ),
                vec![
                    Value::Text(after.to_string()),
                    Value::from(Utc::now()),
                    Value::from(&customer_id),
                    Value::Text(before.to_string()),
                ],
            )
            .await?;
        if affected == 0 {
            // 被并发改了：下一轮重读重算
            continue;
        }
        record(
            ex,
            customer_id,
            before,
            after,
            cause,
            ref_no,
            trigger_source_of(cause),
        )
        .await?;
        tracing::info!(
            customer_id = %customer_id,
            before = %before.round_dp(2),
            after = %after.round_dp(2),
            cause,
            ref_no,
            usage = %usage.summary(),
            "客户信用额度已同步"
        );
        return Ok(CreditSync {
            customer_id,
            before,
            after,
            repaired: true,
        });
    }
    // 三轮都被并发抢先：公式结果没变（同一个库），下一轮事件 / 每日重算会收口。
    let before = stored(ex, customer_id).await?.unwrap_or(Decimal::ZERO);
    let usage = usage_of(ex, customer_id).await?;
    Ok(CreditSync {
        customer_id,
        before,
        after: usage.used,
        repaired: false,
    })
}

/// 触发来源：重算记 `JOB`，其余事件记 `API`（人工动作 / 单据动作）。
fn trigger_source_of(cause: &str) -> &'static str {
    if cause == CAUSE_RECOMPUTE {
        "JOB"
    } else {
        "API"
    }
}

/// 写一条额度变动台账（与业务同事务）。
async fn record(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    before: Decimal,
    after: Decimal,
    cause: &str,
    ref_no: &str,
    trigger_source: &str,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "INSERT INTO {LOG_TABLE} \
                (id, customer_id, before_used, after_used, delta, cause, ref_no, trigger_source, created_at) \
             VALUES (?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,''), ?, ?)"
        ),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&customer_id),
            Value::Text(before.round_dp(2).to_string()),
            Value::Text(after.round_dp(2).to_string()),
            Value::Text((after - before).round_dp(2).to_string()),
            Value::Text(cause.to_string()),
            Value::Text(ref_no.chars().take(40).collect::<String>()),
            Value::Text(trigger_source.to_string()),
            Value::from(Utc::now()),
        ],
    )
    .await?;
    Ok(())
}

/// 订单侧入口：按订单号反查客户再 [`sync`]（订单状态流转调用）。
///
/// 返回 `Ok(None)` = 订单不存在 / 已软删（并发删除），调用方不必因此失败。
pub async fn sync_order(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    cause: &str,
) -> DbResult<Option<CreditSync>> {
    let rows = ex
        .query_sql(
            "SELECT customer_id::text AS customer_id, so_no \
               FROM biz_sales_order WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let customer_id = row
        .get_typed::<Uuid>("customer_id")
        .map_err(|error| DbError::InvalidValue(format!("读销售订单客户: {error}")))?;
    let so_no: String = row
        .get_typed("so_no")
        .map_err(|error| DbError::InvalidValue(format!("读销售订单号: {error}")))?;
    Ok(Some(sync(ex, customer_id, cause, &so_no).await?))
}

/// 重算报告（每日兜底 / 手动「执行一次」都返回它）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecomputeReport {
    /// 扫描的客户数。
    pub scanned: usize,
    /// 修正的客户数（漂移过）。
    pub repaired: usize,
    /// 逐条修正明细（`客户 | 前 | 后`），给日志与验收看。
    pub lines: Vec<String>,
}

impl RecomputeReport {
    /// 一行摘要。
    pub fn summary(&self) -> String {
        format!(
            "信用额度重算：扫描 {} 个客户，修正 {} 个{}",
            self.scanned,
            self.repaired,
            if self.lines.is_empty() {
                String::new()
            } else {
                format!("（{}）", self.lines.join("；"))
            }
        )
    }
}

/// 兜底重算（全局连接版；调度器 / 「执行一次」用）。
pub async fn recompute_all(trigger_source: &str) -> DbResult<RecomputeReport> {
    recompute_with(db(), trigger_source).await
}

/// 兜底重算（吃执行器版，便于测试与事务内补跑）。
///
/// 候选客户 = 「缓存列非 0」∪「有有效订单」∪「有已核销应收」∪「有已过账退货」：
/// 只扫**可能占着额度**的客户，不是全表。每修正一个就记一条 `RECOMPUTE` 台账 + WARN 日志
/// （卡面要求「告警 + 修正 + 记 changelog」）。
pub async fn recompute_with(
    ex: &impl SqlExecutor,
    trigger_source: &str,
) -> DbResult<RecomputeReport> {
    let placeholders = vec!["?"; OCCUPYING_STATUSES.len()].join(", ");
    let params: Vec<Value> = OCCUPYING_STATUSES
        .iter()
        .map(|status| Value::Text((*status).to_string()))
        .collect();
    // SCOPE-EXEMPT: 后台定时对账任务，无操作人；判据是全局一致性，注入部门范围会把
    // 别部门客户的漂移永远修不掉。
    let rows = ex
        .query_sql(
            format!(
                "SELECT c.id::text AS id, c.code AS code \
                   FROM {CUSTOMER_TABLE} c \
                  WHERE c.deleted_at IS NULL AND ( \
                        c.credit_used <> 0 \
                     OR EXISTS (SELECT 1 FROM biz_sales_order o WHERE o.customer_id = c.id \
                                 AND o.deleted_at IS NULL AND o.status IN ({placeholders})) \
                     OR EXISTS (SELECT 1 FROM biz_receivable r WHERE r.customer_id = c.id \
                                 AND r.deleted_at IS NULL AND r.received_amount <> 0) \
                     OR EXISTS (SELECT 1 FROM biz_sales_return t WHERE t.customer_id = c.id \
                                 AND t.deleted_at IS NULL AND t.status = 'POSTED')) \
                  ORDER BY c.code"
            ),
            params,
        )
        .await?;

    let mut report = RecomputeReport::default();
    for row in &rows {
        let id: Uuid = row
            .get_typed("id")
            .map_err(|error| DbError::InvalidValue(format!("读客户 id: {error}")))?;
        let code: String = row.get_typed("code").unwrap_or_default();
        report.scanned += 1;
        let synced = sync(ex, id, CAUSE_RECOMPUTE, &code).await?;
        if synced.repaired {
            report.repaired += 1;
            let line = format!(
                "{} | {} → {}",
                code,
                synced.before.round_dp(2),
                synced.after.round_dp(2)
            );
            tracing::warn!(
                customer_id = %id,
                customer_code = %code,
                before = %synced.before.round_dp(2),
                after = %synced.after.round_dp(2),
                trigger = trigger_source,
                "信用额度漂移已修正（每日重算兜底）"
            );
            report.lines.push(line);
        }
    }
    if report.repaired > 0 {
        tracing::warn!(summary = %report.summary(), "信用额度重算发现并修正漂移");
    } else {
        tracing::info!(summary = %report.summary(), "信用额度重算完成（无漂移）");
    }
    Ok(report)
}

/// 在调度器上注册每日兜底重算（由 `task::Scheduler::register_builtin` 调用）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：客户信用额度兜底重算（漂移即修正 + 记台账，ARCH-7 §15.5）",
        || {
            tokio::spawn(async move {
                match recompute_all(trigger_source_of(CAUSE_RECOMPUTE)).await {
                    Ok(report) => {
                        tracing::info!(summary = %report.summary(), "客户信用额度重算完成")
                    }
                    Err(error) => tracing::error!(error = %error, "客户信用额度重算失败"),
                }
            })
        },
    );
    tracing::info!(
        invoke_target = TASK_NAME,
        cron = TASK_CRON,
        "客户信用额度兜底重算任务已注册"
    );
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("定点数可解析")
    }

    // ---------- 纯函数（不碰库） ----------

    #[test]
    fn net_used_is_ordered_minus_received_minus_returned() {
        assert_eq!(net_used(dec("1000"), dec("0"), dec("0")), dec("1000"));
        assert_eq!(net_used(dec("1000"), dec("400"), dec("0")), dec("600"));
        assert_eq!(net_used(dec("1000"), dec("400"), dec("300")), dec("300"));
        assert_eq!(net_used(dec("1000"), dec("1000"), dec("0")), dec("0"));
    }

    #[test]
    fn net_used_never_goes_negative() {
        // 历史单退货多于在建单：算数结果是负数，口径必须夹到 0（负占用会让下一张单白拿额度）
        assert_eq!(net_used(dec("100"), dec("0"), dec("130")), Decimal::ZERO);
        assert_eq!(
            net_used(dec("100"), dec("200"), dec("0")),
            Decimal::ZERO,
            "收款多于订单（跨单收款 / 预收）也不能出现负占用"
        );
    }

    #[test]
    fn is_same_uses_one_cent_tolerance() {
        assert!(is_same(dec("100.00"), dec("100.00")));
        assert!(
            is_same(dec("100.00"), dec("100.004")),
            "4 位标度的零头算相等"
        );
        assert!(
            is_same(dec("100.00"), dec("100.01")),
            "容差边界 0.01 仍算相等"
        );
        assert!(!is_same(dec("100.00"), dec("100.02")), "超过 0.01 = 漂移");
        assert!(!is_same(dec("0"), dec("0.02")));
    }

    #[test]
    fn cause_of_transition_covers_occupy_and_release() {
        assert_eq!(
            cause_of_transition("SUBMITTED", "APPROVED"),
            CAUSE_ORDER_OCCUPY
        );
        assert_eq!(cause_of_transition("DRAFT", "APPROVED"), CAUSE_ORDER_OCCUPY);
        assert_eq!(
            cause_of_transition("APPROVED", "CANCELLED"),
            CAUSE_ORDER_RELEASE
        );
        assert_eq!(
            cause_of_transition("APPROVED", "CLOSED"),
            CAUSE_ORDER_RELEASE
        );
        assert_eq!(
            cause_of_transition("OUTBOUND", "CLOSED"),
            CAUSE_ORDER_RELEASE
        );
        // 集合内互迁：额度不变，但要留痕（发货推进）
        assert_eq!(
            cause_of_transition("APPROVED", "PARTIAL_OUT"),
            CAUSE_ORDER_STATUS
        );
        assert_eq!(
            cause_of_transition("PARTIAL_OUT", "OUTBOUND"),
            CAUSE_ORDER_STATUS
        );
        // 待审单不进集合
        assert_eq!(
            cause_of_transition("DRAFT", "SUBMITTED"),
            CAUSE_ORDER_STATUS
        );
    }

    #[test]
    fn occupying_statuses_match_arch_15_5_2() {
        // ARCH-7 §15.5.2：APPROVED 起占用；发货两态仍在集合内（货发了钱没到，敞口没少）
        assert_eq!(OCCUPYING_STATUSES, ["APPROVED", "PARTIAL_OUT", "OUTBOUND"]);
        // 待审单只在守卫里算管道占用
        assert_eq!(PENDING_STATUSES, ["SUBMITTED"]);
        assert!(!OCCUPYING_STATUSES.contains(&STATUS_SUBMITTED));
    }

    #[test]
    fn task_name_matches_seeded_invoke_target() {
        // V0313 的 sys_job.invoke_target 与本常量必须逐字一致，否则「执行一次」报 422
        assert_eq!(TASK_NAME, "erp_credit_recompute");
        assert_eq!(TASK_CRON, "0 10 2 * * *");
    }

    // ---------- 真库用例（缺 ERP_DB_URL 时 SKIP，绝不假绿） ----------

    /// 真库用例串行（共用全局 `db()`；重算会扫全库客户，必须避免两组用例互相修漂移）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 执行测试 SQL（`exec_sql` 只收 `String`，测试里到处写 `.to_string()` 太吵）。
    /// 返回 `DbResult`，于是调用点仍是 `.await.expect("...")` 的原形。
    async fn exec(sql: impl AsRef<str>, params: Vec<Value>) -> DbResult<u64> {
        db().exec_sql(sql.as_ref().to_string(), params).await
    }

    /// 初始化全局 db（只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        static ONCE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
        *ONCE
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(error) => {
                            println!("SKIP: set_db 失败（{error}）");
                            false
                        }
                    },
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 夹具客户（`code` 前缀 `CR38-` 便于清场）。
    ///
    /// 先按 `code` 清一遍：上一次跑崩在半路时残留的客户会让 `uk_biz_customer_code` 撞车，
    /// 让用例从「业务失败」变成「跑不起来」（可重复执行是夹具的基本要求）。
    async fn fixture_customer(code: &str) -> Uuid {
        let stale = db()
            .query_sql(
                format!("SELECT id::text AS id FROM {CUSTOMER_TABLE} WHERE code = ?"),
                vec![Value::from(code)],
            )
            .await
            .unwrap_or_default();
        for row in &stale {
            if let Ok(text) = row.get_typed::<String>("id") {
                if let Ok(id) = Uuid::parse_str(&text) {
                    teardown(id).await;
                }
            }
        }
        let id = Uuid::now_v7();
        exec(
            format!(
                "INSERT INTO {CUSTOMER_TABLE} (id, code, name, credit_limit, status) \
                     VALUES (?::uuid, ?, ?, 100000, 'ACTIVE')"
            ),
            vec![
                Value::from(&id),
                Value::from(code),
                Value::from(format!("SO38 夹具客户 {code}")),
            ],
        )
        .await
        .expect("插夹具客户");
        id
    }

    /// 夹具订单（只给 `credit_used` 用到的列）。
    async fn fixture_order(customer: Uuid, status: &str, taxed: &str) -> Uuid {
        let id = Uuid::now_v7();
        exec(
                "INSERT INTO biz_sales_order \
                    (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax) \
                 VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?, ?::text::numeric, 0, ?::text::numeric)",
                vec![
                    Value::from(&id),
                    Value::from(format!("SO38-{}", &id.to_string()[24..])),
                    Value::from(&customer),
                    Value::Text(status.to_string()),
                    Value::Text(taxed.to_string()),
                    Value::Text(taxed.to_string()),
                ],
            )
            .await
            .expect("插夹具订单");
        id
    }

    /// 夹具应收（`received` = 已核销金额）。
    async fn fixture_receivable(customer: Uuid, amount: &str, received: &str) -> Uuid {
        let id = Uuid::now_v7();
        let unsettled = dec(amount) - dec(received);
        let status = if unsettled == Decimal::ZERO {
            "SETTLED"
        } else if dec(received) == Decimal::ZERO {
            "OPEN"
        } else {
            "PARTIAL"
        };
        exec(
                "INSERT INTO biz_receivable \
                    (id, ar_no, customer_id, source_type, source_id, source_no, amount, \
                     received_amount, unsettled_amount, credit_days, period_code, due_date, status) \
                 VALUES (?::uuid, ?, ?::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 30, '2026-11', \
                         CURRENT_DATE + 30, ?)",
                vec![
                    Value::from(&id),
                    Value::from(format!("AR-SO38-{}", &id.to_string()[24..])),
                    Value::from(&customer),
                    Value::Text(amount.to_string()),
                    Value::Text(received.to_string()),
                    Value::Text(unsettled.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("插夹具应收");
        id
    }

    /// 夹具退货（`status` 一般给 `POSTED`）。
    async fn fixture_return(customer: Uuid, status: &str, taxed: &str) -> Uuid {
        let id = Uuid::now_v7();
        exec(
            "INSERT INTO biz_sales_return \
                    (id, return_no, customer_id, return_date, return_type, warehouse_id, status, \
                     total_amount, total_tax, total_with_tax) \
                 VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, 'OTHER', gen_random_uuid(), ?, \
                         ?::text::numeric, 0, ?::text::numeric)",
            vec![
                Value::from(&id),
                Value::from(format!("SR-SO38-{}", &id.to_string()[24..])),
                Value::from(&customer),
                Value::Text(status.to_string()),
                Value::Text(taxed.to_string()),
                Value::Text(taxed.to_string()),
            ],
        )
        .await
        .expect("插夹具退货");
        id
    }

    /// 清场（按 `code` / 单号前缀删，正常 / panic 都清）。
    async fn teardown(customer: Uuid) {
        let _ = exec(
            "DELETE FROM biz_credit_log WHERE customer_id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
            "DELETE FROM biz_sales_order WHERE customer_id = ?::uuid AND so_no LIKE 'SO38-%'",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
            "DELETE FROM biz_receivable WHERE customer_id = ?::uuid AND ar_no LIKE 'AR-SO38-%'",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
                "DELETE FROM biz_sales_return WHERE customer_id = ?::uuid AND return_no LIKE 'SR-SO38-%'",
                vec![Value::from(&customer)],
            )
            .await;
        let _ = exec(
            "DELETE FROM biz_customer WHERE id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await;
    }

    /// 场景一：**下单占用** —— APPROVED 才进计数器，DRAFT / SUBMITTED 不进。
    #[tokio::test]
    async fn scenario_occupy_on_approved_only() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR38-OCCUPY").await;
        let draft = fixture_order(customer, "SUBMITTED", "1000").await;
        let approved = fixture_order(customer, "APPROVED", "1000").await;

        let usage = usage_of(db(), customer).await.expect("算占用");
        assert_eq!(usage.ordered, dec("1000"), "只有 APPROVED 那张进有效订单");
        assert_eq!(usage.used, dec("1000"));

        // 提交待审单不进计数器，但算管道占用（守卫用它）
        let pending = pending_credit(db(), customer, None).await.expect("算管道");
        assert_eq!(pending, dec("1000"), "SUBMITTED 单只在守卫里算");

        let synced = sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T1")
            .await
            .expect("同步占用");
        assert_eq!(synced.after, dec("1000"));
        assert!(synced.repaired);
        assert_eq!(
            stored(db(), customer).await.expect("读缓存"),
            Some(dec("1000"))
        );

        // 幂等：同一事件重放不产生第二条台账
        let again = sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T1")
            .await
            .expect("重复同步");
        assert!(!again.repaired, "容差内相等不写库");
        let logs = db()
            .query_sql(
                format!("SELECT count(*)::text AS n FROM {LOG_TABLE} WHERE customer_id = ?::uuid"),
                vec![Value::from(&customer)],
            )
            .await
            .expect("数台账");
        let n: String = logs[0].get_typed("n").expect("读计数");
        assert_eq!(n, "1", "重放不重复记台账");

        // 订单侧入口按单号反查客户
        let by_order = sync_order(db(), approved, CAUSE_ORDER_STATUS)
            .await
            .expect("订单侧同步")
            .expect("订单存在");
        assert_eq!(by_order.customer_id, customer);
        assert!(!by_order.repaired);
        let _ = draft;
        teardown(customer).await;
    }

    /// 场景二：**收款释放**（主要出口）—— 核销多少释放多少，收清归零。
    #[tokio::test]
    async fn scenario_release_on_settlement() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR38-SETTLE").await;
        fixture_order(customer, "APPROVED", "1000").await;
        sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T2")
            .await
            .expect("先占用");
        assert_eq!(stored(db(), customer).await.unwrap(), Some(dec("1000")));

        // 部分收款 400
        fixture_receivable(customer, "1000", "400").await;
        let partial = sync(db(), customer, CAUSE_AR_SETTLED, "AR-SO38-T2")
            .await
            .expect("部分核销");
        assert_eq!(partial.after, dec("600"), "核销 400 释放 400");

        // 再收 600（收清）
        exec(
            "UPDATE biz_receivable SET received_amount = 1000, unsettled_amount = 0, \
                        status = 'SETTLED', settled_at = now() WHERE customer_id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await
        .expect("改成收清");
        let settled = sync(db(), customer, CAUSE_AR_SETTLED, "AR-SO38-T2")
            .await
            .expect("收清释放");
        assert_eq!(settled.after, Decimal::ZERO, "钱收到了，额度自然释放");

        // 退款反向边：已核销 600 → 又退 600，敞口回来
        exec(
            "UPDATE biz_receivable SET received_amount = 400, unsettled_amount = 600, \
                        status = 'PARTIAL', settled_at = NULL WHERE customer_id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await
        .expect("退款回写");
        let reversed = sync(db(), customer, CAUSE_AR_REVERSED, "RR-SO38-T2")
            .await
            .expect("退款反向");
        assert_eq!(reversed.after, dec("600"), "退款后敞口回到 600");
        teardown(customer).await;
    }

    /// 场景三：**退货释放** —— 只有 POSTED 的退货才释放。
    #[tokio::test]
    async fn scenario_release_on_return_posted() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR38-RETURN").await;
        fixture_order(customer, "APPROVED", "1000").await;
        sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T3")
            .await
            .expect("先占用");

        // 未过账的退货：不释放（货还没回、应收还没冲）
        fixture_return(customer, "APPROVED", "300").await;
        let before = sync(db(), customer, CAUSE_ORDER_STATUS, "SO38-T3")
            .await
            .expect("未过账");
        assert_eq!(before.after, dec("1000"), "APPROVED 的退货不释放额度");

        // 过账：释放 300
        exec(
            "UPDATE biz_sales_return SET status = 'POSTED' \
                  WHERE customer_id = ?::uuid AND status = 'APPROVED'",
            vec![Value::from(&customer)],
        )
        .await
        .expect("过账");
        let posted = sync(db(), customer, CAUSE_RETURN_POSTED, "SR-SO38-T3")
            .await
            .expect("退货过账释放");
        assert_eq!(posted.after, dec("700"), "退 300 释放 300");
        teardown(customer).await;
    }

    /// 场景四：**作废释放** —— 作废 / 关闭的订单出集，整单释放。
    #[tokio::test]
    async fn scenario_release_on_cancel() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR38-CANCEL").await;
        let order = fixture_order(customer, "APPROVED", "1000").await;
        sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T4")
            .await
            .expect("先占用");
        assert_eq!(stored(db(), customer).await.unwrap(), Some(dec("1000")));

        let result = exec(
            "UPDATE biz_sales_order SET status = 'CANCELLED' WHERE id = ?::uuid",
            vec![Value::from(&order)],
        )
        .await
        .expect("作废");
        assert_eq!(result, 1);

        let released = sync(db(), customer, CAUSE_ORDER_RELEASE, "SO38-T4")
            .await
            .expect("作废释放");
        assert_eq!(released.after, Decimal::ZERO, "未发货作废 = 全额释放");
        assert_eq!(released.delta(), dec("-1000"));
        teardown(customer).await;
    }

    /// 场景五：**重算任务能发现并修正漂移**（手工 UPDATE 模拟「漏接出口 / 运维改库」）。
    #[tokio::test]
    async fn scenario_recompute_detects_and_fixes_drift() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR38-DRIFT").await;
        fixture_order(customer, "APPROVED", "1000").await;
        fixture_receivable(customer, "1000", "400").await;
        sync(db(), customer, CAUSE_ORDER_OCCUPY, "SO38-T5")
            .await
            .expect("先占用");
        assert_eq!(stored(db(), customer).await.unwrap(), Some(dec("600")));

        // 模拟漂移：绕过业务写库（v1.0 那种「只占用不释放」的脏数据）
        exec(
            "UPDATE biz_customer SET credit_used = 999 WHERE id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await
        .expect("注入漂移");

        let report = recompute_with(db(), "JOB").await.expect("重算");
        assert!(report.scanned >= 1);
        assert!(report.repaired >= 1, "重算必须发现漂移");
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.starts_with("CR38-DRIFT")),
            "报告要指名哪个客户被修正：{:?}",
            report.lines
        );
        assert_eq!(
            stored(db(), customer).await.unwrap(),
            Some(dec("600")),
            "重算把漂移修正回公式结果"
        );

        // 修正记了 changelog（cause = RECOMPUTE，trigger_source = JOB）
        let rows = db()
            .query_sql(
                format!(
                    "SELECT cause, trigger_source, (after_used - before_used)::text AS delta \
                       FROM {LOG_TABLE} WHERE customer_id = ?::uuid AND cause = ? \
                      ORDER BY created_at DESC LIMIT 1"
                ),
                vec![
                    Value::from(&customer),
                    Value::Text(CAUSE_RECOMPUTE.to_string()),
                ],
            )
            .await
            .expect("查重算台账");
        assert_eq!(rows.len(), 1, "应有一条 RECOMPUTE 台账");
        let trigger: String = rows[0].get_typed("trigger_source").expect("读来源");
        let delta: String = rows[0].get_typed("delta").expect("读差额");
        assert_eq!(trigger, "JOB");
        assert_eq!(dec(&delta), dec("-399"), "999 → 600");

        // 再跑一次：已经一致，不再修正（幂等）
        let again = recompute_with(db(), "JOB").await.expect("二次重算");
        assert!(
            !again
                .lines
                .iter()
                .any(|line| line.starts_with("CR38-DRIFT")),
            "一致后不该再报这条客户"
        );
        teardown(customer).await;
    }
}
