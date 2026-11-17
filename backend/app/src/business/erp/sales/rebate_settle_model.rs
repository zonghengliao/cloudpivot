//! 销售返利结算数据层（M3-16）：`biz_sales_rebate_settlement` 的读写 + 取号 + 基数聚合。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0439__sales_rebate_settlement.up.sql`。
//!
//! # 三条铁律
//!
//! 1. **基数只有一处实现**：[`load_base`] 的 CTE 是本卡唯一的「期间净销售」口径。
//!    订单/退货的白名单常量来自 `rebate_settle_dto`（与销售目标共用 `ORDER_STATUSES`），
//!    金额表达式复用 `sales_target_model::base_amount_expr`（本位币含税）——
//!    别在这里再拼一份 `SUM(total_with_tax)`，多币种客户会静默算错。
//! 2. **作废不物理删**：`status = 'CANCELLED'`，行留着供审计；「未作废」的判据恒为
//!    `deleted_at IS NULL AND status <> 'CANCELLED'`（唯一索引 `uk_biz_srs_period` 同一个谓词）。
//! 3. **数值绑参一律 `?::text::numeric` / `?::text::date`**（驱动不吃 `Decimal` / `NaiveDate` 直接绑）。
//!
//! # 为什么取号必须在事务里
//!
//! [`next_settlement_no`] 推进 `biz_seq.current_no`；推了号却没落库，那个号就永久占用
//! （跳号可接受、重号不可接受）。因此调用方必须在**同一个事务**里「取号 → 插单」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_rebate_settlement 是「客户 × 期间」的结算结果，没有 dept_id /
//!         salesperson_id 归属列，数据范围（SELF / DEPT_*）表达不出来；可见性由
//!         business:erp:rebate-settlement:* 权限码控制。分页走
//!         model_base::page_of → scope::paginate_unscoped（闸①口径）。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::page_of;
use crate::business::erp::sales::rebate_settle_dto::{
    BaseNumbers, ORDER_STATUSES, PolicySnapshot, RETURN_STATUS_POSTED, STATUS_CANCELLED, STATUS_DRAFT,
};
use crate::business::erp::sales::sales_target_model;
use cloudpivot_kernel::business::shared::period_key;
use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 表名 / 列清单 / 排序 / 分页锚点 ====================

/// 结算单表。
pub const TABLE: &str = "biz_sales_rebate_settlement";

/// 号段类型（`biz_seq.seq_type = 'RB'`，V0439 种子）。
pub const SEQ_TYPE: &str = "RB";
/// 单号前缀（与 `seq_type` 同字）。
pub const NO_PREFIX: &str = "RB";
/// 单号流水位数（`RB-yyyyMMdd-000001`）。
pub const NO_SEQ_WIDTH: usize = 6;

/// 结算单查询列（`s` = 结算单，`c` = 客户）。
///
/// `policy_snapshot` 用 `::text` 取出来再 `serde_json` 解析：jsonb 列直接 `get_typed`
/// 在部分驱动路径上会被拒（`linkage::model` 踩过），`::text` 一定拿得到字符串。
const SETTLE_SELECT: &str = "SELECT s.id, s.settlement_no, s.customer_id, \
     c.code AS customer_code, c.name AS customer_name, \
     s.settle_period, s.period_code, s.period_from, s.period_to, \
     s.policy_id, s.policy_no, s.policy_name, s.rule_type, s.rate_unit, \
     s.policy_effective_from, s.policy_effective_to, s.policy_snapshot::text AS policy_snapshot_text, \
     s.order_count, s.order_amount, s.order_qty, s.return_count, s.return_amount, s.return_qty, \
     s.base_amount, s.base_qty, s.tier_seq, s.tier_from, s.tier_to, s.rate, s.rebate_amount, \
     s.status, s.remark, s.created_at, s.updated_at, \
     s.invoice_id, s.invoice_no, s.voucher_id, s.settled_at, s.settled_by \
     FROM biz_sales_rebate_settlement s \
     LEFT JOIN biz_customer c ON c.id = s.customer_id";

/// 分页锚点（结算单表不参与数据范围）。
pub const SETTLE_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_sales_rebate_settlement");

/// 列表排序：期间倒序 → 期间码 → 客户编码 → id（全序，翻页不换行）。
const SETTLE_ORDER: &str = "ORDER BY s.period_code DESC, s.settle_period ASC, c.code ASC, s.id ASC";

// ==================== 行结构 ====================

/// `biz_sales_rebate_settlement` 一行（**带客户编码 / 名称**：列表直接显示，前端不用二次映射）。
#[derive(Debug, Clone)]
pub struct SettlementRow {
    /// 主键。
    pub id: Uuid,
    /// 结算单号。
    pub settlement_no: String,
    /// 客户 id。
    pub customer_id: Uuid,
    /// 客户编码（客户软删后为 `None`）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 结算粒度。
    pub settle_period: String,
    /// 期间码。
    pub period_code: String,
    /// 期间首日（含）。
    pub period_from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub period_to: NaiveDate,
    /// 政策 id。
    pub policy_id: Uuid,
    /// 政策编号（快照）。
    pub policy_no: String,
    /// 政策名称（快照）。
    pub policy_name: String,
    /// 规则类型（快照）。
    pub rule_type: String,
    /// 计价单位（快照）。
    pub rate_unit: String,
    /// 政策生效日（快照）。
    pub policy_effective_from: NaiveDate,
    /// 政策失效日（快照）。
    pub policy_effective_to: Option<NaiveDate>,
    /// 政策全量快照。
    pub policy_snapshot: PolicySnapshot,
    /// 基数（分项 + 净额）。
    pub base: BaseNumbers,
    /// 命中档号。
    pub tier_seq: Option<i16>,
    /// 命中档下限。
    pub tier_from: Decimal,
    /// 命中档上限。
    pub tier_to: Option<Decimal>,
    /// 命中档比率。
    pub rate: Decimal,
    /// 应返金额。
    pub rebate_amount: Decimal,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 红字发票 id（M3-17；未开票为 `None`）。
    pub invoice_id: Option<Uuid>,
    /// 红字发票号快照（M3-17）。
    pub invoice_no: Option<String>,
    /// 红字凭证 id（M3-17）。
    pub voucher_id: Option<Uuid>,
    /// 转结时间（M3-17）。
    pub settled_at: Option<DateTime<Utc>>,
    /// 转结人（M3-17；审计口径，不 join 实时人名）。
    pub settled_by: Option<Uuid>,
}

impl SettlementRow {
    /// 由查询行构造。
    ///
    /// `policy_snapshot_text` 解析失败 → `DbError::InvalidValue`（500）：快照是本卡自己写进去的，
    /// 解析不出来说明库被人手改过，宁可报错也不返回一份「档位空白」的结算单。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let snapshot_text: String = row.get_typed("policy_snapshot_text")?;
        let policy_snapshot: PolicySnapshot = serde_json::from_str(&snapshot_text)
            .map_err(|error| DbError::InvalidValue(format!("policy_snapshot 解析失败：{error}")))?;
        Ok(Self {
            id: row.get_typed("id")?,
            settlement_no: row.get_typed("settlement_no")?,
            customer_id: row.get_typed("customer_id")?,
            customer_code: row.get_typed("customer_code")?,
            customer_name: row.get_typed("customer_name")?,
            settle_period: row.get_typed("settle_period")?,
            period_code: row.get_typed("period_code")?,
            period_from: row.get_typed("period_from")?,
            period_to: row.get_typed("period_to")?,
            policy_id: row.get_typed("policy_id")?,
            policy_no: row.get_typed("policy_no")?,
            policy_name: row.get_typed("policy_name")?,
            rule_type: row.get_typed("rule_type")?,
            rate_unit: row.get_typed("rate_unit")?,
            policy_effective_from: row.get_typed("policy_effective_from")?,
            policy_effective_to: row.get_typed("policy_effective_to")?,
            policy_snapshot,
            base: BaseNumbers {
                order_count: row.get_typed("order_count")?,
                order_amount: row.get_typed("order_amount")?,
                order_qty: row.get_typed("order_qty")?,
                return_count: row.get_typed("return_count")?,
                return_amount: row.get_typed("return_amount")?,
                return_qty: row.get_typed("return_qty")?,
            },
            tier_seq: row.get_typed("tier_seq")?,
            tier_from: row.get_typed("tier_from")?,
            tier_to: row.get_typed("tier_to")?,
            rate: row.get_typed("rate")?,
            rebate_amount: row.get_typed("rebate_amount")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            invoice_id: row.get_typed("invoice_id")?,
            invoice_no: row.get_typed("invoice_no")?,
            voucher_id: row.get_typed("voucher_id")?,
            settled_at: row.get_typed("settled_at")?,
            settled_by: row.get_typed("settled_by")?,
        })
    }

    /// 是否可重算 / 可作废（只有 `DRAFT` 能）。
    pub fn is_draft(&self) -> bool {
        self.status == STATUS_DRAFT
    }
}

/// 「该客户该期间已有的结算单」摘要（试算回显 / 跑批跳过用）。
#[derive(Debug, Clone)]
pub struct ExistingSettleRow {
    /// 主键。
    pub id: Uuid,
    /// 结算单号。
    pub settlement_no: String,
    /// 状态。
    pub status: String,
    /// 应返金额。
    pub rebate_amount: Decimal,
}

/// 覆盖某期间的政策行（跑批的客户清单 + 名字）。
#[derive(Debug, Clone)]
pub struct CoveringPolicyRow {
    /// 政策 id。
    pub policy_id: Uuid,
    /// 政策编号。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 客户 id。
    pub customer_id: Uuid,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
}

/// 建单入参（`base` 的分项与净额一起落库：净额是「当时算的」，别指望以后重算能复现）。
#[derive(Debug, Clone)]
pub struct NewSettlement<'a> {
    /// 结算单号（[`next_settlement_no`] 的返回值）。
    pub settlement_no: &'a str,
    /// 客户。
    pub customer_id: Uuid,
    /// 结算粒度。
    pub settle_period: &'a str,
    /// 期间码。
    pub period_code: &'a str,
    /// 期间首日（含）。
    pub period_from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub period_to: NaiveDate,
    /// 政策 id。
    pub policy_id: Uuid,
    /// 政策编号（快照）。
    pub policy_no: &'a str,
    /// 政策名称（快照）。
    pub policy_name: &'a str,
    /// 规则类型（快照）。
    pub rule_type: &'a str,
    /// 计价单位（快照）。
    pub rate_unit: &'a str,
    /// 政策生效日（快照）。
    pub policy_effective_from: NaiveDate,
    /// 政策失效日（快照）。
    pub policy_effective_to: Option<NaiveDate>,
    /// 政策全量快照（JSON 文本）。
    pub policy_snapshot: String,
    /// 基数。
    pub base: BaseNumbers,
    /// 命中档号。
    pub tier_seq: i16,
    /// 命中档下限。
    pub tier_from: Decimal,
    /// 命中档上限。
    pub tier_to: Option<Decimal>,
    /// 命中档比率。
    pub rate: Decimal,
    /// 应返金额。
    pub rebate_amount: Decimal,
    /// 备注。
    pub remark: Option<&'a str>,
}

// ==================== 单号（纯逻辑 + 取号） ====================

/// 组装单号（`RB-{周期键}-{6 位流水}`）。
pub fn format_settlement_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 拆单号：`RB-yyyyMMdd-NNNNNN` → `(周期键, 流水)`。任何一条不符 → `None`。
pub fn parse_settlement_no(value: &str) -> Option<(String, i64)> {
    let rest = value.strip_prefix(NO_PREFIX)?.strip_prefix('-')?;
    let (period, seq) = rest.split_once('-')?;
    if period.len() != 8 || !period.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = period.get(0..4)?.parse().ok()?;
    let month: u32 = period.get(4..6)?.parse().ok()?;
    let day: u32 = period.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    if seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let no: i64 = seq.parse().ok()?;
    if no < 1 {
        return None;
    }
    Some((period.to_string(), no))
}

/// `biz_seq(seq_type='RB')` 缺失时幂等补一行（正常库有 V0439 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'RB', '返利结算单编号', 'RB', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '返利结算单编号：RB-yyyyMMdd-000001，按天（CST）滚动（V0439 · M3-16）' \
         WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'RB' AND q.deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='RB')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'RB' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个结算单号（周期键按北京时间）。
///
/// 调用方必须在**同一个事务**里调用本函数 + 插结算单。
pub async fn next_settlement_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key::period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_settlement_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='RB') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 基数聚合（本卡唯一口径实现） ====================

/// 订单状态白名单 → SQL `IN (...)`（状态来自常量，不含用户输入，字面量拼接安全）。
fn order_status_in() -> String {
    ORDER_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 退货含税金额 → 本位币的折算表达式（`r` = 退货、`ar` = 所属应收）。
///
/// 退货表没有 `base_*` 列（V0314 只给订单/应收/收款/发票加了），而返利基数必须是本位币
/// （多币种客户把 USD 当 CNY 相加会静默算错）。折算是「用所属应收的『本位币 / 原币』比例」：
/// 那个比例就是建单时锁定的汇率，退货没有自己的汇率。
///
/// ⚠ 应收的金额列叫 `amount`（原币价税合计）/ `base_total_amount`（本位币），
/// **不是** `total_with_tax`（那是订单/退货/发票的列名）。写错列名 PG 只回一句
/// 「column ar.total_with_tax does not exist」，而驱动把它吞成「query failed: : db error」。
///
/// 拿不到应收（`receivable_id` 为空，或应收已被清成 0）时**回落原币** ——
/// 本位币单的 `rate = 1`，两者相等；多币种且无应收属于历史脏单，回落原币只是不减错方向。
fn return_amount_expr() -> &'static str {
    "CASE WHEN ar.amount > 0 AND ar.base_total_amount > 0 \
       THEN ROUND(r.total_with_tax * ar.base_total_amount / ar.amount, 4) \
       ELSE r.total_with_tax END"
}

/// 期间内某客户的销售 / 退货基数（**一条 SQL 六个标量**，不做 N 次往返）。
///
/// # 口径（与 `rebate_settle_dto` 模块头逐条对应）
///
/// ```text
/// 订单：deleted_at IS NULL + status ∈ ORDER_STATUSES + order_date ∈ [from, to)
///       金额 = sales_target_model::base_amount_expr()（本位币含税，0 时回落原币）
///       数量 = SUM(biz_sales_order_item.qty)
/// 退货：deleted_at IS NULL + status = 'POSTED' + return_date ∈ [from, to)
///       金额 = 折本位币（见 return_amount_expr）
///       数量 = SUM(biz_sales_return_item.qty)
/// ```
///
/// # 异常情况
///
/// 聚合失败原样上抛 [`DbError`]。空期间返回全 0 的 [`BaseNumbers`]（不报错：
/// 「这个月没卖」是合法业务事实，应返 0 由 DTO 层给提示）。
pub async fn load_base(customer_id: Uuid, from: NaiveDate, to: NaiveDate) -> DbResult<BaseNumbers> {
    let amount_expr = sales_target_model::base_amount_expr();
    let sql = format!(
        "WITH ord AS ( \
             SELECT o.id, {amount_expr} AS amt \
               FROM biz_sales_order o \
              WHERE o.deleted_at IS NULL AND o.customer_id = ?::uuid \
                AND o.status IN ({}) \
                AND o.order_date >= ?::text::date AND o.order_date < ?::text::date \
         ), ord_qty AS ( \
             SELECT COALESCE(SUM(i.qty), 0) AS qty \
               FROM ord JOIN biz_sales_order_item i ON i.so_id = ord.id \
         ), ret AS ( \
             SELECT r.id, {} AS amt \
               FROM biz_sales_return r \
               LEFT JOIN biz_receivable ar ON ar.id = r.receivable_id AND ar.deleted_at IS NULL \
              WHERE r.deleted_at IS NULL AND r.customer_id = ?::uuid \
                AND r.status = '{RETURN_STATUS_POSTED}' \
                AND r.return_date >= ?::text::date AND r.return_date < ?::text::date \
         ), ret_qty AS ( \
             SELECT COALESCE(SUM(ri.qty), 0) AS qty \
               FROM ret JOIN biz_sales_return_item ri ON ri.return_id = ret.id \
         ) \
         SELECT (SELECT count(*) FROM ord) AS order_count, \
                (SELECT COALESCE(SUM(amt), 0) FROM ord) AS order_amount, \
                (SELECT qty FROM ord_qty) AS order_qty, \
                (SELECT count(*) FROM ret) AS return_count, \
                (SELECT COALESCE(SUM(amt), 0) FROM ret) AS return_amount, \
                (SELECT qty FROM ret_qty) AS return_qty",
        order_status_in(),
        return_amount_expr()
    );
    let rows = db()
        .query_sql(
            sql,
            vec![
                Value::from(&customer_id),
                Value::Text(from.to_string()),
                Value::Text(to.to_string()),
                Value::from(&customer_id),
                Value::Text(from.to_string()),
                Value::Text(to.to_string()),
            ],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| DbError::InvalidValue("基数聚合没有返回行（不该发生）".to_string()))?;
    Ok(BaseNumbers {
        order_count: row.get_typed("order_count")?,
        order_amount: row.get_typed("order_amount")?,
        order_qty: row.get_typed("order_qty")?,
        return_count: row.get_typed("return_count")?,
        return_amount: row.get_typed("return_amount")?,
        return_qty: row.get_typed("return_qty")?,
    })
}

// ==================== 查询 ====================

/// 按主键取结算单（作废的行**照样返回**：详情要能看见「这单被作废了」）。
pub async fn find_settlement(id: Uuid) -> DbResult<Option<SettlementRow>> {
    settlement_of(db(), id).await
}

/// 指定执行器版本。
pub async fn settlement_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SettlementRow>> {
    let rows = ex
        .query_sql(
            format!("{SETTLE_SELECT} WHERE s.deleted_at IS NULL AND s.id = ?::uuid"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(SettlementRow::from_row).transpose()
}

/// 在事务里**锁住**一条结算单（`SELECT … FOR UPDATE`）。
///
/// 必须在事务里调用；不存在 / 已软删 → `false`（调用方报 404）。
/// 只用裸表 `SELECT id`：带 `LEFT JOIN` 的查询加 `FOR UPDATE` 会报
/// 「FOR UPDATE cannot be applied to the nullable side of an outer join」。
pub async fn lock_settlement(ex: &impl SqlExecutor, id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 结算单无归属列，锁单行不产出用户可见行集。
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_sales_rebate_settlement WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    Ok(!rows.is_empty())
}

/// 取「该客户该粒度该期间**未作废**的结算单」（唯一索引 `uk_biz_srs_period` 的同一谓词）。
pub async fn alive_of_period(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    settle_period: &str,
    period_code: &str,
) -> DbResult<Option<ExistingSettleRow>> {
    // SCOPE-EXEMPT: 按 (客户, 粒度, 期间) 取单行，不产出列表。
    let rows = ex
        .query_sql(
            "SELECT id, settlement_no, status, rebate_amount \
               FROM biz_sales_rebate_settlement \
              WHERE customer_id = ?::uuid AND settle_period = ? AND period_code = ? \
                AND deleted_at IS NULL AND status <> ? \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&customer_id),
                Value::from(settle_period),
                Value::from(period_code),
                Value::from(STATUS_CANCELLED),
            ],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok::<ExistingSettleRow, DbError>(ExistingSettleRow {
                id: row.get_typed("id")?,
                settlement_no: row.get_typed("settlement_no")?,
                status: row.get_typed("status")?,
                rebate_amount: row.get_typed("rebate_amount")?,
            })
        })
        .transpose()
}

/// 该政策被多少张**未作废**的结算单引用（政策软删前的引用闸）。
pub async fn alive_count_by_policy(ex: &impl SqlExecutor, policy_id: Uuid) -> DbResult<i64> {
    // SCOPE-EXEMPT: 计数查询，不产出用户可见行集。
    let rows = ex
        .query_sql(
            format!(
                "SELECT count(*) AS n FROM {TABLE} \
                  WHERE policy_id = ?::uuid AND deleted_at IS NULL AND status <> ?"
            ),
            vec![Value::from(&policy_id), Value::from(STATUS_CANCELLED)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("n"))
        .transpose()
        .map(|value| value.unwrap_or(0))
}

/// 取「覆盖整期且启用」的政策清单（跑批的客户来源）。
///
/// # 口径
///
/// `status = 1` + `effective_from <= 期间首日` + (`effective_to IS NULL` 或
/// `effective_to >= 期间末日`)。**覆盖整期**是硬条件：半期套一份政策算出来的金额有争议
/// （见 `rebate_settle_dto` 模块头「为什么要覆盖整期」），所以跑批直接不收这类政策。
///
/// `customer_ids` 非空时只取这些客户（跑批可选收窄）。
pub async fn policies_covering_period(
    settle_period: &str,
    from: NaiveDate,
    last_day: NaiveDate,
    customer_ids: &[Uuid],
) -> DbResult<Vec<CoveringPolicyRow>> {
    let mut sql = String::from(
        "SELECT p.id AS policy_id, p.policy_no, p.policy_name, p.customer_id, \
                c.code AS customer_code, c.name AS customer_name \
           FROM biz_sales_rebate_policy p \
           LEFT JOIN biz_customer c ON c.id = p.customer_id \
          WHERE p.deleted_at IS NULL AND p.status = 1 AND p.settle_period = ? \
            AND p.effective_from <= ?::text::date \
            AND (p.effective_to IS NULL OR p.effective_to >= ?::text::date)",
    );
    let params = vec![
        Value::from(settle_period),
        Value::Text(from.to_string()),
        Value::Text(last_day.to_string()),
    ];
    if !customer_ids.is_empty() {
        let literals = customer_ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND p.customer_id IN ({literals})"));
    }
    sql.push_str(" ORDER BY c.code ASC NULLS LAST, p.policy_no ASC");
    // SCOPE-EXEMPT: 跑批取的是「政策清单」，不是用户可见的业务列表（分页另有 list 接口）。
    let rows = db().query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        out.push(CoveringPolicyRow {
            policy_id: row.get_typed("policy_id")?,
            policy_no: row.get_typed("policy_no")?,
            policy_name: row.get_typed("policy_name")?,
            customer_id: row.get_typed("customer_id")?,
            customer_code: row.get_typed("customer_code")?,
            customer_name: row.get_typed("customer_name")?,
        });
    }
    Ok(out)
}

/// 该客户该粒度**最近一份启用政策**的有效期窗口（只用于「期间没被覆盖」的报错文案）。
#[derive(Debug, Clone)]
pub struct PolicyWindow {
    /// 政策编号。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期。
    pub effective_to: Option<NaiveDate>,
}

/// 取该客户该粒度最近一份启用政策（**不管日期**）。
///
/// 只在 [`crate::business::erp::sales::rebate_settle_handler::load_policy`] 报 `66091` 时用：
/// 「首日没有生效政策」有两种原因（压根没建 / 建了但还没生效），报文要能说清是哪一种，
/// 用户才知道该去「新建」还是「改生效日」。
pub async fn policy_window(customer_id: Uuid, settle_period: &str) -> DbResult<Option<PolicyWindow>> {
    // SCOPE-EXEMPT: 单行取数（报错文案用），不产出列表。
    let rows = db()
        .query_sql(
            "SELECT policy_no, policy_name, effective_from, effective_to \
               FROM biz_sales_rebate_policy \
              WHERE deleted_at IS NULL AND status = 1 AND customer_id = ?::uuid AND settle_period = ? \
              ORDER BY effective_from DESC, policy_no DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&customer_id), Value::from(settle_period)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok::<PolicyWindow, DbError>(PolicyWindow {
                policy_no: row.get_typed("policy_no")?,
                policy_name: row.get_typed("policy_name")?,
                effective_from: row.get_typed("effective_from")?,
                effective_to: row.get_typed("effective_to")?,
            })
        })
        .transpose()
}

/// 分页筛选入参。
#[derive(Debug, Clone, Copy, Default)]
pub struct SettlePageQuery<'a> {
    /// 关键字（结算单号 / 客户编码 / 客户名称 / 政策编号 模糊）。
    pub keyword: Option<&'a str>,
    /// 客户精确。
    pub customer_id: Option<Uuid>,
    /// 结算粒度精确（调用方已归一化）。
    pub settle_period: Option<&'a str>,
    /// 期间码精确（调用方已归一化）。
    pub period_code: Option<&'a str>,
    /// 状态精确（调用方已校验）。
    pub status: Option<&'a str>,
    /// 政策精确。
    pub policy_id: Option<Uuid>,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 分页：关键字模糊 + 客户 / 粒度 / 期间 / 状态 / 政策精确。
pub async fn settle_page(query: &SettlePageQuery<'_>) -> DbResult<(Vec<SettlementRow>, i64)> {
    let mut sql = format!("{SETTLE_SELECT} WHERE s.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(query.keyword) {
        sql.push_str(" AND (s.settlement_no LIKE ? OR c.code LIKE ? OR c.name LIKE ? OR s.policy_no LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(customer_id) = query.customer_id {
        sql.push_str(" AND s.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(settle_period) = query.settle_period {
        sql.push_str(" AND s.settle_period = ?");
        params.push(Value::from(settle_period));
    }
    if let Some(period_code) = query.period_code {
        sql.push_str(" AND s.period_code = ?");
        params.push(Value::from(period_code));
    }
    if let Some(status) = query.status {
        sql.push_str(" AND s.status = ?");
        params.push(Value::from(status));
    }
    if let Some(policy_id) = query.policy_id {
        sql.push_str(" AND s.policy_id = ?::uuid");
        params.push(Value::from(&policy_id));
    }
    page_of(
        SETTLE_TARGET,
        sql,
        params,
        SETTLE_ORDER,
        query.page,
        query.page_size,
        SettlementRow::from_row,
    )
    .await
}

// ==================== 写入 ====================

/// 计算落库：**取号 → 插单**（调用方负责事务边界）。
///
/// 返回 `(新单 id, 单号)`。
pub async fn insert_settlement(
    ex: &impl SqlExecutor,
    new: &NewSettlement<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, settlement_no, customer_id, settle_period, period_code, period_from, period_to, \
                 policy_id, policy_no, policy_name, rule_type, rate_unit, \
                 policy_effective_from, policy_effective_to, policy_snapshot, \
                 order_count, order_amount, order_qty, return_count, return_amount, return_qty, \
                 base_amount, base_qty, tier_seq, tier_from, tier_to, rate, rebate_amount, \
                 status, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?::uuid, ?, ?, ?::text::date, ?::text::date, \
                 ?::uuid, ?, ?, ?, ?, \
                 ?::text::date, NULLIF(?::text,'')::date, ?::jsonb, \
                 ?::text::int4, ?::text::numeric, ?::text::numeric, ?::text::int4, \
                 ?::text::numeric, ?::text::numeric, \
                 ?::text::numeric, ?::text::numeric, ?::text::smallint, ?::text::numeric, \
                 NULLIF(?::text,'')::numeric, ?::text::numeric, ?::text::numeric, \
                 ?, NULLIF(?::text,''), ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::from(new.settlement_no.to_string()),
            Value::from(&new.customer_id),
            Value::from(new.settle_period.to_string()),
            Value::from(new.period_code.to_string()),
            Value::Text(new.period_from.to_string()),
            Value::Text(new.period_to.to_string()),
            Value::from(&new.policy_id),
            Value::from(new.policy_no.to_string()),
            Value::from(new.policy_name.to_string()),
            Value::from(new.rule_type.to_string()),
            Value::from(new.rate_unit.to_string()),
            Value::Text(new.policy_effective_from.to_string()),
            Value::Text(new.policy_effective_to.map(|day| day.to_string()).unwrap_or_default()),
            Value::Json(new.policy_snapshot.clone()),
            Value::Text(new.base.order_count.to_string()),
            Value::Text(new.base.order_amount.to_string()),
            Value::Text(new.base.order_qty.to_string()),
            Value::Text(new.base.return_count.to_string()),
            Value::Text(new.base.return_amount.to_string()),
            Value::Text(new.base.return_qty.to_string()),
            Value::Text(new.base.net_amount().to_string()),
            Value::Text(new.base.net_qty().to_string()),
            Value::Text(new.tier_seq.to_string()),
            Value::Text(new.tier_from.to_string()),
            Value::Text(new.tier_to.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(new.rate.to_string()),
            Value::Text(new.rebate_amount.to_string()),
            Value::from(STATUS_DRAFT.to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    Ok(id)
}

/// 重算：覆盖基数、政策快照、命中档与应返金额（**只对 `DRAFT` 生效**）。
///
/// 返回影响行数（0 = 单据在这中间被作废/转应付，调用方按 409 处理）。
pub async fn update_calc(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewSettlement<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET policy_id = ?::uuid, policy_no = ?, policy_name = ?, rule_type = ?, rate_unit = ?, \
                    policy_effective_from = ?::text::date, \
                    policy_effective_to = NULLIF(?::text,'')::date, policy_snapshot = ?::jsonb, \
                    order_count = ?::text::int4, order_amount = ?::text::numeric, \
                    order_qty = ?::text::numeric, return_count = ?::text::int4, \
                    return_amount = ?::text::numeric, return_qty = ?::text::numeric, \
                    base_amount = ?::text::numeric, base_qty = ?::text::numeric, \
                    tier_seq = ?::text::smallint, tier_from = ?::text::numeric, \
                    tier_to = NULLIF(?::text,'')::numeric, rate = ?::text::numeric, \
                    rebate_amount = ?::text::numeric, \
                    remark = NULLIF(?::text,''), updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
        ),
        vec![
            Value::from(&new.policy_id),
            Value::from(new.policy_no.to_string()),
            Value::from(new.policy_name.to_string()),
            Value::from(new.rule_type.to_string()),
            Value::from(new.rate_unit.to_string()),
            Value::Text(new.policy_effective_from.to_string()),
            Value::Text(new.policy_effective_to.map(|day| day.to_string()).unwrap_or_default()),
            Value::Json(new.policy_snapshot.clone()),
            Value::Text(new.base.order_count.to_string()),
            Value::Text(new.base.order_amount.to_string()),
            Value::Text(new.base.order_qty.to_string()),
            Value::Text(new.base.return_count.to_string()),
            Value::Text(new.base.return_amount.to_string()),
            Value::Text(new.base.return_qty.to_string()),
            Value::Text(new.base.net_amount().to_string()),
            Value::Text(new.base.net_qty().to_string()),
            Value::Text(new.tier_seq.to_string()),
            Value::Text(new.tier_from.to_string()),
            Value::Text(new.tier_to.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(new.rate.to_string()),
            Value::Text(new.rebate_amount.to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::from(STATUS_DRAFT),
        ],
    )
    .await
}

/// 作废：`DRAFT → CANCELLED`（**行不删**：留着供审计，同期间可重开一张）。
///
/// 返回影响行数（0 = 不是 `DRAFT`（已转应付/已作废）或行不存在，调用方按 409/404 处理）。
pub async fn mark_cancelled(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    remark: Option<&str>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET status = ?, remark = COALESCE(NULLIF(?::text,''), remark), \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
        ),
        vec![
            Value::from(STATUS_CANCELLED),
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::from(STATUS_DRAFT),
        ],
    )
    .await
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 分页锚点必须是「不参与数据权限」的（结算单没有归属列）。
    #[test]
    fn settle_target_is_unscoped() {
        assert_eq!(SETTLE_TARGET.table, "biz_sales_rebate_settlement");
        assert!(!SETTLE_TARGET.is_scoped(), "返利结算是全局结果表：不参与数据范围注入");
        println!("[M3-16] 分页锚点 biz_sales_rebate_settlement 为 unscoped ✓");
    }

    /// 单号格式与解析互逆（写歪一个字母，按单号回溯就废了）。
    #[test]
    fn settlement_no_roundtrip() {
        let no = format_settlement_no("20261117", 7);
        assert_eq!(no, "RB-20261117-000007");
        assert_eq!(parse_settlement_no(&no), Some(("20261117".to_string(), 7)));
        for bad in [
            "",
            "RB",
            "RB-20261117",
            "BR-20261117-000001",
            "RB-2026-11-17-1",
            "RB-20261117-000000",
            "RB-20261317-000001",
        ] {
            assert_eq!(parse_settlement_no(bad), None, "非法单号必须解析失败：{bad}");
        }
        println!("[M3-16] 单号 RB-yyyyMMdd-NNNNNN 组装/解析互逆，7 种非法形状全拒 ✓");
    }

    /// 排序必须带 id 收全序（否则翻页换行）。
    #[test]
    fn page_order_is_total() {
        assert!(SETTLE_ORDER.contains("s.period_code DESC"));
        assert!(SETTLE_ORDER.ends_with("s.id ASC"), "必须以 id 收全序：{SETTLE_ORDER}");
        println!("[M3-16] 列表全序：期间倒序 → 粒度 → 客户编码 → id ✓");
    }

    /// 订单白名单与退货白名单的 SQL 片段来自常量（不是手写字符串）。
    #[test]
    fn status_whitelists_come_from_constants() {
        let orders = order_status_in();
        for status in ORDER_STATUSES {
            assert!(orders.contains(status), "订单白名单缺 {status}：{orders}");
        }
        assert!(!orders.contains("DRAFT"), "草稿订单不算销售");
        assert!(!orders.contains("CANCELLED"), "作废订单不算销售");
        assert!(return_amount_expr().contains("ar.base_total_amount"));
        println!("[M3-16] 订单白名单 4 状态 / 退货折本位币表达式均来自常量 ✓");
    }

    /// `DRAFT` 才可重算/作废。
    #[test]
    fn only_draft_is_editable() {
        let mut row = SettlementRow {
            id: Uuid::nil(),
            settlement_no: String::new(),
            customer_id: Uuid::nil(),
            customer_code: None,
            customer_name: None,
            settle_period: "MONTH".to_string(),
            period_code: "2026-10".to_string(),
            period_from: NaiveDate::from_ymd_opt(2026, 10, 1).expect("合法日期"),
            period_to: NaiveDate::from_ymd_opt(2026, 11, 1).expect("合法日期"),
            policy_id: Uuid::nil(),
            policy_no: String::new(),
            policy_name: String::new(),
            rule_type: "BY_QTY".to_string(),
            rate_unit: "PER_UNIT".to_string(),
            policy_effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期"),
            policy_effective_to: None,
            policy_snapshot: PolicySnapshot {
                policy_no: String::new(),
                policy_name: String::new(),
                rule_type: "BY_QTY".to_string(),
                rate_unit: "PER_UNIT".to_string(),
                effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期"),
                effective_to: None,
                tiers: Vec::new(),
            },
            base: BaseNumbers::default(),
            tier_seq: Some(1),
            tier_from: Decimal::ZERO,
            tier_to: None,
            rate: Decimal::ZERO,
            rebate_amount: Decimal::ZERO,
            status: STATUS_DRAFT.to_string(),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            invoice_id: None,
            invoice_no: None,
            voucher_id: None,
            settled_at: None,
            settled_by: None,
        };
        assert!(row.is_draft());
        row.status = STATUS_CANCELLED.to_string();
        assert!(!row.is_draft(), "作废后不可再重算");
        println!("[M3-16] 只有 DRAFT 可重算/可作废 ✓");
    }
}
