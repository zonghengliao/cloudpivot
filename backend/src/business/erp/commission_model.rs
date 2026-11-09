//! 佣金结算 · 数据层（销售链 · SO-36 / ARCH-7 §15.4.3）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0309__sales_commission.up.sql`
//! （三表）+ `V0311__commission_settle_setup.up.sql`（有符号基数 / 2241.02 / CM 号段）。
//!
//! # 「已收款」到底取哪一列
//!
//! `biz_receivable.received_amount` —— 它由收款单核销（`biz_receipt_allocation`）与
//! 收款退款单（`biz_receipt_refund_allocation`）**双向维护**，是「真正收到钱」的唯一
//! 台账列（`unsettled_amount = amount - received_amount` 由库层 CHECK 恒等）。
//! 不取 `amount`（开单金额）、也不取 `biz_receipt`（收款单可能是预收，没核销到这张应收）。
//!
//! # 应收 → 订单 的链路（四跳，每一跳都有索引）
//!
//! ```text
//! biz_receivable (source_type='SALES_OUT', source_id) → biz_stock_out（销售出库）
//!   → biz_stock_out.source_id (source_type='SALES')   → biz_shipment
//!   → biz_shipment.so_id                              → biz_sales_order.salesperson_id
//! ```
//!
//! 为什么不在应收上再存一个 `so_id`：ADR-06 单据链零外键基线，应收只认「来源单据」，
//! 订单归属由来源链推导；这条 JOIN 全走主键，且计提是批处理（一人一次），不是高频点查。
//!
//! # 「已认领基数」= 防重复发钱的闸
//!
//! 同一笔已收款不能在两张结算单里各计一次佣。故每次计提都先算：
//!
//! ```text
//! 已认领 = SUM(biz_commission_item.base_amount)  ← 只统计**存活**结算单的明细
//! 本期可计佣 = 应收.received_amount − 已认领
//! ```
//!
//! 存活 = `biz_commission_settlement.deleted_at IS NULL`。**草稿也算认领**：否则
//! 「草稿没审 + 下个月再提一次」会把同一笔钱算两遍；草稿被弃用时先软删它，
//! 基数自然释放（重算走的正是这条路）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::commission_dto::{self, CommissionFilter, STATUS_APPROVED, STATUS_DRAFT, STATUS_PAID};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

// ==================== 常量 ====================

/// 结算单表。
pub const TABLE: &str = "biz_commission_settlement";
/// 佣金明细表。
pub const ITEM_TABLE: &str = "biz_commission_item";
/// 佣金规则表。
pub const RULE_TABLE: &str = "biz_commission_rule";

/// 应收单表。
pub const RECEIVABLE_TABLE: &str = "biz_receivable";
/// 销售出库单表。
pub const STOCK_OUT_TABLE: &str = "biz_stock_out";
/// 发货单表。
pub const SHIPMENT_TABLE: &str = "biz_shipment";
/// 销售订单表。
pub const ORDER_TABLE: &str = "biz_sales_order";

/// 应收的来源类型：销售出库（`receivable_bridge::SOURCE_TYPE_SALES_OUT`）。
pub const RECEIVABLE_SOURCE_SALES_OUT: &str = "SALES_OUT";
/// 销售出库单的来源类型：发货单（`shipment_posting` 建单时写死）。
pub const STOCK_OUT_SOURCE_SALES: &str = "SALES";

/// 结算单号前缀 / 号段。
pub const NO_PREFIX: &str = "CM";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "CM";
/// 会计科目：销售费用（本仓小企业科目集，卡面 6601 不存在）。
pub const ACCOUNT_EXPENSE: &str = "5601";
/// 会计科目：其他应付款-佣金（V0311 种子，末级）。
pub const ACCOUNT_PAYABLE: &str = "2241.02";
/// 会计科目：银行存款（发放时贷方）。
pub const ACCOUNT_BANK: &str = "1002";

/// 列表锚点：`salesperson_id` 是归属列（SELF 范围用），无部门列
/// （部门类范围由 `filter_for` 安全侧降级为「仅本人」）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::new(TABLE, "t", None, Some("salesperson_id"));

// ==================== 行 ====================

/// 结算单行。
#[derive(Debug, Clone)]
pub struct SettlementRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub settlement_no: String,
    /// 销售员。
    pub salesperson_id: Uuid,
    /// 期间。
    pub period_code: String,
    /// 基数合计。
    pub total_sales: Decimal,
    /// 佣金合计。
    pub total_commission: Decimal,
    /// 状态。
    pub status: String,
    /// 发放时间。
    pub paid_at: Option<DateTime<Utc>>,
    /// 计提凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl SettlementRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            settlement_no: row.get_typed("settlement_no")?,
            salesperson_id: row.get_typed("salesperson_id")?,
            period_code: row.get_typed("period_code")?,
            total_sales: row.get_typed("total_sales")?,
            total_commission: row.get_typed("total_commission")?,
            status: row.get_typed("status")?,
            paid_at: row.get_typed("paid_at")?,
            voucher_id: row.get_typed("voucher_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }

    /// 是否已入账（APPROVED / PAID）：这两种状态不许重算。
    pub fn is_posted(&self) -> bool {
        self.status == STATUS_APPROVED || self.status == STATUS_PAID
    }
}

/// 计提候选：一张应收 + 它落在哪个销售员 / 订单上。
#[derive(Debug, Clone)]
pub struct CandidateRow {
    /// 应收 id。
    pub receivable_id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 已核销（收到钱）金额。
    pub received_amount: Decimal,
    /// 已被存活结算单认领的基数（有符号）。
    pub settled_base: Decimal,
    /// 销售订单 id。
    pub so_id: Uuid,
    /// 销售订单号。
    pub so_no: String,
}

impl CandidateRow {
    /// 本期可计佣基数（正 = 新收钱；负 = 冲回；0 = 无事可做）。
    pub fn delta(&self) -> Decimal {
        (self.received_amount - self.settled_base).round_dp(4)
    }
}

/// 佣金规则行（只读快照）。
#[derive(Debug, Clone)]
pub struct RuleRow {
    /// 主键。
    pub id: Uuid,
    /// 规则名。
    pub rule_name: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象。
    pub scope_id: Option<Uuid>,
    /// 计算方式。
    pub calc_type: String,
    /// 比率 / 固定额。
    pub rate: Decimal,
    /// 起算金额。
    pub min_amount: Decimal,
    /// 封顶。
    pub max_amount: Option<Decimal>,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含；空 = 长期）。
    pub effective_to: Option<NaiveDate>,
    /// 创建时间（同级规则的确定性次序末键之一）。
    pub created_at: DateTime<Utc>,
}

/// 订单行（计提按物料拆分基数用）。
#[derive(Debug, Clone)]
pub struct OrderLineRow {
    /// 订单 id。
    pub so_id: Uuid,
    /// 物料 id（订单行必填，但历史脏数据可能为空，按 `Option` 读）。
    pub material_id: Option<Uuid>,
    /// 未税金额。
    pub amount: Decimal,
    /// 数量。
    pub qty: Decimal,
}

/// 一条待落库的明细。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 应收 id。
    pub receivable_id: Uuid,
    /// 订单 id。
    pub so_id: Uuid,
    /// 命中的规则。
    pub rule_id: Option<Uuid>,
    /// 基数（有符号）。
    pub base_amount: Decimal,
    /// 比率快照。
    pub rate: Decimal,
    /// 佣金。
    pub commission: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 读 ====================

const COLUMNS: &str = "id, settlement_no, salesperson_id, period_code, total_sales, \
                       total_commission, status, paid_at, voucher_id, remark, created_at";

/// 按主键取结算单（未软删）。
pub async fn find_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SettlementRow>> {
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(SettlementRow::from_row).transpose()
}

/// 按主键锁结算单（状态迁移串行化）。
pub async fn lock_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SettlementRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(SettlementRow::from_row).transpose()
}

/// 取「某销售员某期间」的存活结算单（唯一索引保证至多一行）。
pub async fn alive_settlement(
    ex: &impl SqlExecutor,
    salesperson_id: Uuid,
    period_code: &str,
) -> DbResult<Option<SettlementRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE salesperson_id = ?::uuid AND period_code = ? AND deleted_at IS NULL"
            ),
            vec![Value::from(&salesperson_id), Value::Text(period_code.to_string())],
        )
        .await?;
    row.as_ref().map(SettlementRow::from_row).transpose()
}

/// 明细行。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 订单 id。
    pub so_id: Option<Uuid>,
    /// 应收 id。
    pub receivable_id: Option<Uuid>,
    /// 规则 id。
    pub rule_id: Option<Uuid>,
    /// 基数。
    pub base_amount: Decimal,
    /// 比率。
    pub rate: Decimal,
    /// 佣金。
    pub commission: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

impl ItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            so_id: row.get_typed("so_id")?,
            receivable_id: row.get_typed("receivable_id")?,
            rule_id: row.get_typed("rule_id")?,
            base_amount: row.get_typed("base_amount")?,
            rate: row.get_typed("rate")?,
            commission: row.get_typed("commission")?,
            remark: row.get_typed("remark")?,
        })
    }
}

/// 取结算单全部明细（计提在前、冲回在后，同额按基数倒序）。
pub async fn items_of(ex: &impl SqlExecutor, settlement_id: Uuid) -> DbResult<Vec<ItemRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT id, so_id, receivable_id, rule_id, base_amount, rate, commission, remark \
                   FROM {ITEM_TABLE} WHERE settlement_id = ?::uuid \
                  ORDER BY (commission >= 0) DESC, base_amount DESC, created_at ASC, id ASC"
            ),
            vec![Value::from(&settlement_id)],
        )
        .await?;
    rows.iter().map(ItemRow::from_row).collect()
}

/// 结算单上挂的计提凭证号。
pub async fn voucher_no_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<Option<String>> {
    let row = ex
        .query_first_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    row.map(|row| row.get_typed::<String>("voucher_no")).transpose()
}

/// 「有销售员归属的订单」的销售员清单（结算批跑的目标集合）。
pub async fn salespeople_with_orders(ex: &impl SqlExecutor) -> DbResult<Vec<(Uuid, String)>> {
    // SCOPE-EXEMPT: 这是计提批跑的候选枚举（服务端内部动作），不是面向用户的列表；
    // 可见性由 business:erp:commission-settle:create 权限码控制。
    let rows = ex
        .query_sql(
            format!(
                "SELECT DISTINCT o.salesperson_id AS id, \
                        COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name \
                   FROM {ORDER_TABLE} o \
                   LEFT JOIN sys_user u ON u.id = o.salesperson_id \
                  WHERE o.salesperson_id IS NOT NULL AND o.deleted_at IS NULL \
                  ORDER BY 2, 1"
            ),
            vec![],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_typed("id")?, row.get_typed("name")?)))
        .collect()
}

/// 某销售员的计提候选（应收 + 订单 + 已认领基数）。
pub async fn load_candidates(ex: &impl SqlExecutor, salesperson_id: Uuid) -> DbResult<Vec<CandidateRow>> {
    let sql = format!(
        "SELECT r.id AS receivable_id, r.ar_no, r.received_amount, s.so_id, so.so_no, \
                COALESCE(base.settled, 0) AS settled_base \
           FROM {RECEIVABLE_TABLE} r \
           JOIN {STOCK_OUT_TABLE} o ON o.id = r.source_id AND o.source_type = ? \
           JOIN {SHIPMENT_TABLE} s ON s.id = o.source_id \
           JOIN {ORDER_TABLE} so ON so.id = s.so_id \
           LEFT JOIN ( \
                SELECT i.receivable_id, SUM(i.base_amount) AS settled \
                  FROM {ITEM_TABLE} i \
                  JOIN {TABLE} t ON t.id = i.settlement_id \
                 WHERE t.deleted_at IS NULL AND i.receivable_id IS NOT NULL \
                 GROUP BY i.receivable_id \
           ) base ON base.receivable_id = r.id \
          WHERE r.deleted_at IS NULL AND r.source_type = ? \
            AND so.salesperson_id = ?::uuid AND so.deleted_at IS NULL \
          ORDER BY r.ar_no"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(STOCK_OUT_SOURCE_SALES.to_string()),
                Value::Text(RECEIVABLE_SOURCE_SALES_OUT.to_string()),
                Value::from(&salesperson_id),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(CandidateRow {
                receivable_id: row.get_typed("receivable_id")?,
                ar_no: row.get_typed("ar_no")?,
                received_amount: row.get_typed("received_amount")?,
                settled_base: row.get_typed("settled_base")?,
                so_id: row.get_typed("so_id")?,
                so_no: row.get_typed("so_no")?,
            })
        })
        .collect()
}

/// 全部启用中的佣金规则（规则量级小，一次读全 + Rust 里匹配，避免 N 次查询）。
pub async fn load_rules(ex: &impl SqlExecutor) -> DbResult<Vec<RuleRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT id, rule_name, scope_type, scope_id, calc_type, rate, min_amount, \
                        max_amount, effective_from, effective_to, created_at \
                   FROM {RULE_TABLE} \
                  WHERE deleted_at IS NULL AND status = 'ACTIVE' \
                  ORDER BY effective_from DESC, created_at DESC, id"
            ),
            vec![],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(RuleRow {
                id: row.get_typed("id")?,
                rule_name: row.get_typed("rule_name")?,
                scope_type: row.get_typed("scope_type")?,
                scope_id: row.get_typed("scope_id")?,
                calc_type: row.get_typed("calc_type")?,
                rate: row.get_typed("rate")?,
                min_amount: row.get_typed("min_amount")?,
                max_amount: row.get_typed("max_amount")?,
                effective_from: row.get_typed("effective_from")?,
                effective_to: row.get_typed("effective_to")?,
                created_at: row.get_typed("created_at")?,
            })
        })
        .collect()
}

/// 一批订单的全部明细（按物料拆基数用）。
pub async fn load_order_lines(ex: &impl SqlExecutor, so_ids: &[Uuid]) -> DbResult<Vec<OrderLineRow>> {
    if so_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?::uuid"; so_ids.len()].join(", ");
    let sql = format!(
        "SELECT so_id, material_id, amount, qty FROM biz_sales_order_item \
          WHERE so_id IN ({placeholders}) ORDER BY so_id, line_no"
    );
    let params: Vec<Value> = so_ids.iter().map(Value::from).collect();
    let rows = ex.query_sql(sql, params).await?;
    rows.iter()
        .map(|row| {
            Ok(OrderLineRow {
                so_id: row.get_typed("so_id")?,
                material_id: row.get_typed("material_id")?,
                amount: row.get_typed("amount")?,
                qty: row.get_typed("qty")?,
            })
        })
        .collect()
}

/// 分页（数据权限：`salesperson_id` 为归属列，走 `scope::paginate` 唯一入口）。
pub async fn page(
    principal: &ScopedPrincipal,
    q: &CommissionFilter,
) -> DbResult<(Vec<(SettlementRow, String, i64)>, i64)> {
    let mut sql = format!(
        "SELECT t.id, t.settlement_no, t.salesperson_id, t.period_code, t.total_sales, \
                t.total_commission, t.status, t.paid_at, t.voucher_id, t.remark, t.created_at, \
                COALESCE(NULLIF(u.real_name, ''), u.username, '') AS salesperson_name, \
                (SELECT count(*) FROM {ITEM_TABLE} i WHERE i.settlement_id = t.id) AS item_count \
           FROM {TABLE} t \
           LEFT JOIN sys_user u ON u.id = t.salesperson_id \
          WHERE t.deleted_at IS NULL {}",
        scope::FILTER_MARKER
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = q.period_code.as_deref() {
        sql.push_str(" AND t.period_code = ?");
        params.push(Value::Text(period.to_string()));
    }
    if let Some(id) = q.salesperson_id {
        sql.push_str(" AND t.salesperson_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pattern) = crate::utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(
            " AND (t.settlement_no LIKE ? \
                    OR COALESCE(NULLIF(u.real_name, ''), u.username, '') LIKE ? \
                    OR u.username LIKE ?)",
        );
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    let result = scope::paginate(
        LIST_TARGET,
        &sql,
        params,
        Some("ORDER BY t.period_code DESC, t.settlement_no DESC"),
        q.page,
        q.page_size,
        principal,
    )
    .await?;
    let mut out = Vec::with_capacity(result.list.len());
    for row in &result.list {
        let name: String = row.get_typed("salesperson_name")?;
        let count: i64 = row.get_typed("item_count")?;
        out.push((SettlementRow::from_row(row)?, name, count));
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 写 ====================

/// 取下一个结算单号：`CM-yyyyMM-000001`（**按月**滚动，与凭证的按天滚动不同）。
///
/// 周期键 = `period_code` 去掉连字符（`202611`）。三分支与 `voucher::bump_seq` 同形：
/// 同月累加 / 新月份重置 / 时钟回拨（period_key 更大）**绝不**重置 —— 回拨重置会撞号。
pub async fn next_settlement_no(ex: &impl SqlExecutor, period_code: &str) -> DbResult<String> {
    let period_key = period_code.replace('-', "");
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(SEQ_TYPE.to_string()),
            ],
        )
        .await?;
    let no: i64 = rows
        .first()
        .ok_or_else(|| {
            DbError::InvalidValue(format!(
                "取号失败：biz_seq(seq_type='{SEQ_TYPE}') 行不存在（V0311 未应用？）"
            ))
        })?
        .get_typed("current_no")?;
    Ok(format!("{NO_PREFIX}-{period_key}-{no:06}"))
}

/// 软删一张结算单（重算前清场；明细随 `deleted_at` 一起失效于「已认领基数」口径）。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 落一张结算单（DRAFT），返回 id。
#[allow(clippy::too_many_arguments)]
pub async fn insert_settlement(
    ex: &impl SqlExecutor,
    settlement_no: &str,
    salesperson_id: Uuid,
    period_code: &str,
    total_sales: Decimal,
    total_commission: Decimal,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, settlement_no, salesperson_id, period_code, total_sales, total_commission, \
                 status, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?::uuid, ?, ?::text::numeric, ?::text::numeric, ?, ?, ?, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(settlement_no.to_string()),
            Value::from(&salesperson_id),
            Value::Text(period_code.to_string()),
            Value::Text(total_sales.to_string()),
            Value::Text(total_commission.to_string()),
            Value::Text(STATUS_DRAFT.to_string()),
            remark.map(|v| Value::Text(v.to_string())).unwrap_or(Value::Null),
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
        ],
    )
    .await?;
    Ok(id)
}

/// 批量落明细。
pub async fn insert_items(
    ex: &impl SqlExecutor,
    settlement_id: Uuid,
    items: &[NewItem],
    now: DateTime<Utc>,
) -> DbResult<usize> {
    if items.is_empty() {
        return Ok(0);
    }
    let mut params: Vec<Value> = Vec::with_capacity(items.len() * 11);
    let mut tuples: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        tuples.push(
            "(?, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, \
              NULLIF(?::text,'')::uuid, ?::text::numeric, ?::text::numeric, ?, ?, ?)"
                .to_string(),
        );
        params.push(Value::from(&Uuid::now_v7()));
        params.push(Value::from(&settlement_id));
        params.push(Value::from(&item.receivable_id));
        params.push(Value::from(&item.so_id));
        params.push(Value::Text(item.base_amount.to_string()));
        // 可空 uuid 统一走 NULLIF(?::text,'')：本栈驱动不支持把 NULL 直接绑到 uuid 位。
        params.push(Value::Text(item.rule_id.map(|v| v.to_string()).unwrap_or_default()));
        params.push(Value::Text(item.rate.to_string()));
        params.push(Value::Text(item.commission.to_string()));
        params.push(
            item.remark
                .as_deref()
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
        );
        params.push(Value::from(now));
        params.push(Value::from(now));
    }
    let sql = format!(
        "INSERT INTO {ITEM_TABLE} \
            (id, settlement_id, receivable_id, so_id, base_amount, rule_id, rate, commission, \
             remark, created_at, updated_at) VALUES {}",
        tuples.join(", ")
    );
    let affected = ex.exec_sql(sql, params).await?;
    Ok(usize::try_from(affected).unwrap_or(0))
}

/// 状态迁移落库（乐观锁：`WHERE status = 原状态`）+ 回写凭证 / 发放时间。
///
/// 参数多于 7 个：这里是**唯一**的状态落库入口，拆成结构体只会把「哪个字段忘了传」
/// 从编译期挪到运行期；`clippy::too_many_arguments` 在此显式豁免并写明理由。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    paid_at: Option<DateTime<Utc>>,
    voucher_id: Option<Uuid>,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // 可空列一律「按需拼进 SQL」，不把 NULL 绑到强类型占位符上（驱动对 NULL 的类型推断不稳）。
    let mut sql = format!(
        "UPDATE {TABLE} SET status = ?, updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid"
    );
    let mut params: Vec<Value> = vec![
        Value::Text(to.to_string()),
        Value::from(now),
        Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
    ];
    if let Some(ts) = paid_at {
        sql.push_str(", paid_at = ?");
        params.push(Value::from(ts));
    }
    if let Some(value) = voucher_id {
        sql.push_str(", voucher_id = ?::uuid");
        params.push(Value::from(&value));
    }
    sql.push_str(
        ", remark = CASE WHEN NULLIF(?::text,'') IS NULL THEN remark \
                         WHEN remark IS NULL OR remark = '' THEN ?::text \
                         ELSE remark || E'\\n' || ?::text END",
    );
    for _ in 0..3 {
        params.push(Value::Text(remark.unwrap_or_default().to_string()));
    }
    sql.push_str(" WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL");
    params.push(Value::from(&id));
    params.push(Value::Text(from.to_string()));
    ex.exec_sql(sql, params).await
}

/// 订单号映射（明细回显用）。
pub async fn order_nos(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = vec!["?::uuid"; ids.len()].join(", ");
    let rows = ex
        .query_sql(
            format!("SELECT id, so_no FROM {ORDER_TABLE} WHERE id IN ({placeholders})"),
            ids.iter().map(Value::from).collect(),
        )
        .await?;
    let mut map = std::collections::HashMap::new();
    for row in &rows {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("so_no")?);
    }
    Ok(map)
}

/// 应收单号映射（明细回显用）。
pub async fn receivable_nos(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = vec!["?::uuid"; ids.len()].join(", ");
    let rows = ex
        .query_sql(
            format!("SELECT id, ar_no FROM {RECEIVABLE_TABLE} WHERE id IN ({placeholders})"),
            ids.iter().map(Value::from).collect(),
        )
        .await?;
    let mut map = std::collections::HashMap::new();
    for row in &rows {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("ar_no")?);
    }
    Ok(map)
}

/// 状态域常量自检用：`APPROVED` / `PAID` 的字面量与迁移 CHECK 一致。
pub fn posted_statuses() -> [&'static str; 2] {
    [STATUS_APPROVED, STATUS_PAID]
}

/// DTO 里的状态白名单（handler 校验用，避免重复 import）。
pub fn statuses() -> [&'static str; 3] {
    commission_dto::STATUSES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_is_received_minus_claimed() {
        let mut row = CandidateRow {
            receivable_id: Uuid::now_v7(),
            ar_no: "AR-1".to_string(),
            received_amount: Decimal::new(10000, 2),
            settled_base: Decimal::new(3000, 2),
            so_id: Uuid::now_v7(),
            so_no: "SO-1".to_string(),
        };
        assert_eq!(row.delta(), Decimal::new(7000, 2));
        // 冲回：已认领 100，已收 40 → 负差额
        row.settled_base = Decimal::new(10000, 2);
        row.received_amount = Decimal::new(4000, 2);
        assert_eq!(row.delta(), Decimal::new(-6000, 2));
        // 无变化
        row.settled_base = row.received_amount;
        assert_eq!(row.delta(), Decimal::ZERO);
    }

    #[test]
    fn posted_states_are_not_draft() {
        assert_eq!(posted_statuses(), ["APPROVED", "PAID"]);
        assert!(!posted_statuses().contains(&STATUS_DRAFT));
    }

    #[test]
    fn status_domain_is_the_dto_one() {
        assert_eq!(statuses(), ["DRAFT", "APPROVED", "PAID"]);
    }

    #[test]
    fn accounts_are_repo_convention_codes() {
        // 卡面写 6601 销售费用，本仓科目集没有 66xx（V0183 / V0226 头注释），用 5601。
        assert_eq!(ACCOUNT_EXPENSE, "5601");
        assert_eq!(ACCOUNT_PAYABLE, "2241.02");
        assert_eq!(ACCOUNT_BANK, "1002");
        assert_eq!(SEQ_TYPE, "CM");
        assert_eq!(NO_PREFIX, "CM");
    }

    #[test]
    fn list_target_uses_salesperson_as_owner_column() {
        assert!(LIST_TARGET.is_scoped(), "佣金结算单必须参与数据权限");
        assert_eq!(LIST_TARGET.owner_col, Some("salesperson_id"));
        assert_eq!(LIST_TARGET.dept_col, None, "结算单没有部门列（降级为仅本人）");
    }
}
