//! 销售返利政策数据层（M3-15）：`biz_sales_rebate_policy` / `biz_sales_rebate_rule` 的读写 + 取号。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0438__sales_rebate_policy.up.sql`。
//!
//! # 三条铁律（与 V0438 的建表注释一一对应）
//!
//! 1. **软删不物理删**：`deleted_at IS NULL` 贯穿所有查询；删掉 = 政策从列表消失、单号可复用。
//! 2. **规则是「整单替换」**：改政策 = 覆盖政策头 + 删旧规则 + 插新规则，同一个事务。
//!    逐档 UPDATE 会让「3 档改 2 档」留下一条孤儿档位（旧第 3 档没人指它，却还在表里），
//!    而 M3-16 按 `tier_from` 落档时会读到它（两个档位上界相同 → 命中哪档看执行计划）。
//! 3. **数值绑参一律 `?::text::numeric`**（驱动不吃 `Decimal` 直接绑）；档位 / 比率的小数位
//!    已在 `rebate_dto` 校验过，本层不重复判。
//!
//! # 重叠检查必须与写入在**同一事务**、且在**同一把锁**下
//!
//! [`lock_customer`]（`pg_advisory_xact_lock(hashtextextended(customer_id, 0))`）→
//! [`overlap_of`] 查重叠 → 写。顺序不能反：先查再锁的话，两个并发建单会双双通过检查，
//! 之后同一个结算周期能按两份政策各算一次返利（返利翻倍且不报错）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_rebate_policy / biz_sales_rebate_rule 是政策配置表（V0438 无
//!         dept_id / 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//!         business:erp:rebate-policy:* 权限码控制。分页走
//!         model_base::page_of → scope::paginate_unscoped（闸①口径）。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::page_of;
use crate::business::erp::sales::rebate_dto::{RuleWrite, STATUS_ENABLED};
use cloudpivot_kernel::business::shared::period_key;
use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 表名 / 列清单 / 排序 / 分页锚点 ====================

/// 政策头表。
pub const TABLE: &str = "biz_sales_rebate_policy";
/// 规则表。
pub const RULE_TABLE: &str = "biz_sales_rebate_rule";

/// 号段类型（`biz_seq.seq_type = 'BR'`，V0438 种子）。
pub const SEQ_TYPE: &str = "BR";
/// 单号前缀（与 `seq_type` 同字）。
pub const NO_PREFIX: &str = "BR";
/// 单号流水位数（`BR-yyyyMMdd-000001`）。
pub const NO_SEQ_WIDTH: usize = 6;

/// 政策行查询列（`p` = 政策，`c` = 客户）。
const POLICY_SELECT: &str = "SELECT p.id, p.policy_no, p.policy_name, p.customer_id, \
     c.code AS customer_code, c.name AS customer_name, \
     p.rule_type, p.rate_unit, p.settle_period, p.effective_from, p.effective_to, \
     p.status, p.remark, p.created_at, p.updated_at \
     FROM biz_sales_rebate_policy p \
     LEFT JOIN biz_customer c ON c.id = p.customer_id";

/// 规则列清单。
const RULE_COLUMNS: &str = "id, policy_id, seq, tier_from, tier_to, rate, remark";

/// 分页锚点（全局配置表，不参与数据范围）。
pub const POLICY_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_sales_rebate_policy");

/// 列表排序：生效日近的在前 → 政策号 → id（全序，翻页不换行）。
const POLICY_ORDER: &str = "ORDER BY p.effective_from DESC, p.policy_no DESC, p.id ASC";

// ==================== 行结构 ====================

/// `biz_sales_rebate_policy` 一行（**带客户编码 / 名称**：列表直接显示，前端不用二次映射）。
#[derive(Debug, Clone)]
pub struct RebatePolicyRow {
    /// 主键。
    pub id: Uuid,
    /// 政策编号（`BR-yyyyMMdd-000001`）。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 客户 id（弱引用 `biz_customer.id`）。
    pub customer_id: Uuid,
    /// 客户编码（客户软删后为 `None`：`LEFT JOIN` 不吞行）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 规则类型（`BY_QTY` / `BY_AMOUNT` / `TIERED`）。
    pub rule_type: String,
    /// 计价单位（`PER_UNIT` / `PERCENT`）。
    pub rate_unit: String,
    /// 结算粒度（`MONTH` / `QUARTER`）。
    pub settle_period: String,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期有效。
    pub effective_to: Option<NaiveDate>,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl RebatePolicyRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            policy_no: row.get_typed("policy_no")?,
            policy_name: row.get_typed("policy_name")?,
            customer_id: row.get_typed("customer_id")?,
            customer_code: row.get_typed("customer_code")?,
            customer_name: row.get_typed("customer_name")?,
            rule_type: row.get_typed("rule_type")?,
            rate_unit: row.get_typed("rate_unit")?,
            settle_period: row.get_typed("settle_period")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 该政策在 `today` 这一天是否生效（启用 + 落在有效期内）。
    ///
    /// 「已失效」是日期事实，不落状态列（V0438 建表注释同一句）：这里现算，供列表打标签、
    /// 供 M3-16 判断「这一单该不该吃这份政策」。
    pub fn is_active_on(&self, today: NaiveDate) -> bool {
        self.status == STATUS_ENABLED && self.effective_from <= today && self.effective_to.is_none_or(|to| to >= today)
    }
}

/// `biz_sales_rebate_rule` 一行。
#[derive(Debug, Clone)]
pub struct RebateRuleRow {
    /// 主键。
    pub id: Uuid,
    /// 政策。
    pub policy_id: Uuid,
    /// 档号（1 起）。
    pub seq: i16,
    /// 本档下限（含）。
    pub tier_from: Decimal,
    /// 本档上限（不含）；`None` = 无上限。
    pub tier_to: Option<Decimal>,
    /// 比率。
    pub rate: Decimal,
    /// 本档备注。
    pub remark: Option<String>,
}

impl RebateRuleRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            policy_id: row.get_typed("policy_id")?,
            seq: row.get_typed("seq")?,
            tier_from: row.get_typed("tier_from")?,
            tier_to: row.get_typed("tier_to")?,
            rate: row.get_typed("rate")?,
            remark: row.get_typed("remark")?,
        })
    }

    /// 折算成纯逻辑用的规则行（[`rebate_dto::pick_tier`] 的入参类型）。
    pub fn to_write(&self) -> RuleWrite {
        RuleWrite {
            seq: self.seq,
            tier_from: self.tier_from,
            tier_to: self.tier_to,
            rate: self.rate,
            remark: self.remark.clone(),
        }
    }
}

/// 建 / 改政策的单头字段（`customer_id` 与规则由调用方另给）。
#[derive(Debug, Clone)]
pub struct NewPolicy<'a> {
    /// 政策名称。
    pub policy_name: &'a str,
    /// 客户。
    pub customer_id: Uuid,
    /// 规则类型。
    pub rule_type: &'a str,
    /// 计价单位。
    pub rate_unit: &'a str,
    /// 结算粒度。
    pub settle_period: &'a str,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 失效日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: i16,
    /// 备注（空串由 SQL 转 NULL）。
    pub remark: Option<&'a str>,
}

// ==================== 单号（纯逻辑 + 取号） ====================

/// 组装单号（`BR-{周期键}-{6 位流水}`）。
pub fn format_policy_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 拆单号：`BR-yyyyMMdd-NNNNNN` → `(周期键, 流水)`。任何一条不符 → `None`。
pub fn parse_policy_no(value: &str) -> Option<(String, i64)> {
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

/// `biz_seq(seq_type='BR')` 缺失时幂等补一行（正常库有 V0438 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'BR', '返利政策编号', 'BR', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '返利政策编号：BR-yyyyMMdd-000001，按天（CST）滚动（V0438 · M3-15）' \
         WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'BR' AND q.deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='BR')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'BR' AND deleted_at IS NULL \
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

/// 取一个政策编号（周期键按北京时间）。
///
/// 调用方必须在**同一个事务**里调用本函数 + 写政策 + 写规则：取号推了 `current_no`
/// 却没落库，那个号就永久占用（跳号可接受，重号不可接受）。
pub async fn next_policy_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key::period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_policy_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='BR') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 查询 ====================

/// 按主键取政策（软删行不返回）。`None` = 不存在或已软删。
pub async fn find_policy(id: Uuid) -> DbResult<Option<RebatePolicyRow>> {
    policy_of(db(), id).await
}

/// 在事务里**锁住**一条政策行（`SELECT … FOR UPDATE`），把同一张政策上的并发改动串行化。
///
/// 必须在事务里调用。不存在 / 已软删 → `None`（调用方报 404）。
///
/// 只用裸表 `SELECT id`：带 `LEFT JOIN` 的查询加 `FOR UPDATE` 会报
/// 「FOR UPDATE cannot be applied to the nullable side of an outer join」。
pub async fn lock_policy(ex: &impl SqlExecutor, id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 政策表无归属列，锁单行不产出用户可见行集。
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_sales_rebate_policy WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    Ok(!rows.is_empty())
}

/// 取一条政策：`None` = 不存在或已软删。
pub async fn policy_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<RebatePolicyRow>> {
    let rows = ex
        .query_sql(
            format!("{POLICY_SELECT} WHERE p.deleted_at IS NULL AND p.id = ?::uuid"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(RebatePolicyRow::from_row).transpose()
}

/// 按客户 + 结算粒度 + 有效期取**启用**政策（M3-16 结算取政策用；本卡只提供，不算金额）。
///
/// 口径与 [`overlap_of`] 一致：`effective_from <= day <= COALESCE(effective_to, ∞)`、
/// `status = 1`。重叠检查保证同一 (客户, 粒度) 最多命中一条。
pub async fn active_policy_of_customer(
    customer_id: Uuid,
    settle_period: &str,
    day: NaiveDate,
) -> DbResult<Option<RebatePolicyRow>> {
    let rows = db()
        .query_sql(
            format!(
                "{POLICY_SELECT} WHERE p.deleted_at IS NULL AND p.status = 1 \
                   AND p.customer_id = ?::uuid AND p.settle_period = ? \
                   AND p.effective_from <= ?::text::date \
                   AND (p.effective_to IS NULL OR p.effective_to >= ?::text::date) \
                 ORDER BY p.effective_from DESC LIMIT 1"
            ),
            vec![
                Value::from(&customer_id),
                Value::from(settle_period),
                Value::Text(day.to_string()),
                Value::Text(day.to_string()),
            ],
        )
        .await?;
    rows.first().map(RebatePolicyRow::from_row).transpose()
}

/// 分页筛选入参。
#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyPageQuery<'a> {
    /// 关键字（政策编号 / 政策名称 / 客户编码 / 客户名称 模糊）。
    pub keyword: Option<&'a str>,
    /// 客户精确。
    pub customer_id: Option<Uuid>,
    /// 规则类型精确（调用方已归一化）。
    pub rule_type: Option<&'a str>,
    /// 结算粒度精确（调用方已归一化）。
    pub settle_period: Option<&'a str>,
    /// 状态精确（调用方已校验 1/0）。
    pub status: Option<i16>,
    /// 「在指定日期生效」筛选（现算，不落状态列）。
    pub active_on: Option<NaiveDate>,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 分页：关键字模糊 + 客户 / 类型 / 粒度 / 状态精确 + `active_on` 现算生效。
pub async fn policy_page(query: &PolicyPageQuery<'_>) -> DbResult<(Vec<RebatePolicyRow>, i64)> {
    let mut sql = format!("{POLICY_SELECT} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(query.keyword) {
        sql.push_str(" AND (p.policy_no LIKE ? OR p.policy_name LIKE ? OR c.code LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(customer_id) = query.customer_id {
        sql.push_str(" AND p.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(rule_type) = query.rule_type {
        sql.push_str(" AND p.rule_type = ?");
        params.push(Value::from(rule_type));
    }
    if let Some(settle_period) = query.settle_period {
        sql.push_str(" AND p.settle_period = ?");
        params.push(Value::from(settle_period));
    }
    if let Some(status) = query.status {
        sql.push_str(" AND p.status = ?::text::smallint");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(day) = query.active_on {
        // 「这一天生效」= 启用 + 落在有效期内（现算，与 RebatePolicyRow::is_active_on 同口径）
        sql.push_str(
            " AND p.status = 1 AND p.effective_from <= ?::text::date \
              AND (p.effective_to IS NULL OR p.effective_to >= ?::text::date)",
        );
        params.push(Value::Text(day.to_string()));
        params.push(Value::Text(day.to_string()));
    }
    page_of(
        POLICY_TARGET,
        sql,
        params,
        POLICY_ORDER,
        query.page,
        query.page_size,
        RebatePolicyRow::from_row,
    )
    .await
}

/// 取一条政策的规则（按 `seq` 升序）。
pub async fn rules_of(policy_id: Uuid) -> DbResult<Vec<RebateRuleRow>> {
    rules_of_in(db(), policy_id).await
}

/// 取规则的指定执行器版本（详情 / 编辑回显都要在同一个快照里读）。
pub async fn rules_of_in(ex: &impl SqlExecutor, policy_id: Uuid) -> DbResult<Vec<RebateRuleRow>> {
    let rows = ex
        .query_sql(
            format!("SELECT {RULE_COLUMNS} FROM {RULE_TABLE} WHERE policy_id = ?::uuid ORDER BY seq ASC"),
            vec![Value::from(&policy_id)],
        )
        .await?;
    rows.iter().map(RebateRuleRow::from_row).collect()
}

/// 一次取多张政策的规则（列表页用：**一条 SQL**，不做 N+1）。
///
/// 返回按 `policy_id` 分组的 `Vec<(policy_id, rules)>`，组内按 `seq` 升序；
/// 入参里的政策若一条规则都没有（不该发生，但库被绕过接口写过就可能），不会出现在返回里。
pub async fn rules_of_many(ids: &[Uuid]) -> DbResult<Vec<(Uuid, Vec<RebateRuleRow>)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let payload = serde_json::to_string(&ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
        .map_err(|error| DbError::InvalidValue(format!("政策 id 列表序列化失败：{error}")))?;
    let rows = db()
        .query_sql(
            format!(
                "SELECT r.id, r.policy_id, r.seq, r.tier_from, r.tier_to, r.rate, r.remark \
                   FROM {RULE_TABLE} r \
                   JOIN jsonb_array_elements_text(?::jsonb) AS v(pid) \
                     ON r.policy_id = v.pid::uuid \
                  ORDER BY r.policy_id, r.seq ASC"
            ),
            vec![Value::Json(payload)],
        )
        .await?;
    let mut grouped: Vec<(Uuid, Vec<RebateRuleRow>)> = Vec::new();
    for row in rows.iter() {
        let rule = RebateRuleRow::from_row(row)?;
        match grouped.last_mut() {
            Some((policy_id, rules)) if *policy_id == rule.policy_id => rules.push(rule),
            _ => grouped.push((rule.policy_id, vec![rule])),
        }
    }
    Ok(grouped)
}

// ==================== 重叠检查 ====================

/// 一条与本次写入冲突的启用政策（`None` = 不冲突）。
#[derive(Debug, Clone)]
pub struct PolicyConflict {
    /// 冲突政策的主键。
    pub id: Uuid,
    /// 冲突政策的编号（报错文案用）。
    pub policy_no: String,
    /// 冲突政策的名称。
    pub policy_name: String,
    /// 冲突政策的生效日。
    pub effective_from: NaiveDate,
    /// 冲突政策的失效日。
    pub effective_to: Option<NaiveDate>,
}

/// 先取「客户级」事务锁：同一客户的返利政策写入串行化。
///
/// `pg_advisory_xact_lock` 在事务结束（提交/回滚）时自动释放，不需要手工解锁。
/// key 用 `hashtextextended(uuid文本, 0)`：同一客户恒定、不同客户几乎不撞。
///
/// **必须在 `db().begin()` 之后调用**：不在事务里时锁会立刻释放，等于没加。
pub async fn lock_customer(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<()> {
    // SCOPE-EXEMPT: advisory lock 不读写业务行。
    ex.exec_sql(
        "SELECT pg_advisory_xact_lock(hashtextextended(?::text, 0))".to_string(),
        vec![Value::Text(customer_id.to_string())],
    )
    .await?;
    Ok(())
}

/// 查「同一客户 × 同一结算粒度」是否已有**启用的**政策与本次有效期重叠。
///
/// # 参数
///
/// - `ex`：执行器（**必须**是已取 [`lock_customer`] 的那个事务）。
/// - `customer_id` / `settle_period`：分组维度（月度与季度政策允许并存）。
/// - `from` / `to`：本次的有效期（`to = None` 表示长期）。
/// - `exclude_id`：编辑时排除自己。
///
/// # 返回值
///
/// 冲突的第一条政策（按生效日升序，便于报错文案稳定）；`None` = 不冲突。
///
/// # 口径
///
/// 只有**启用**的政策参与比较（`status = 0` 的政策停用即不结算，允许与新的重叠 ——
/// 这正是「先停用再改」的操作路径）。区间按「闭区间 + 长期无限」比：
/// `effective_to IS NULL OR effective_to >= from` 且 `to IS NULL OR effective_from <= to`。
pub async fn overlap_of(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    settle_period: &str,
    from: NaiveDate,
    to: Option<NaiveDate>,
    exclude_id: Option<Uuid>,
) -> DbResult<Option<PolicyConflict>> {
    let mut sql = String::from(
        "SELECT id, policy_no, policy_name, effective_from, effective_to \
           FROM biz_sales_rebate_policy \
          WHERE deleted_at IS NULL AND status = 1 \
            AND customer_id = ?::uuid AND settle_period = ? \
            AND (effective_to IS NULL OR effective_to >= ?::text::date) \
            AND (NULLIF(?::text,'')::date IS NULL OR effective_from <= NULLIF(?::text,'')::date)",
    );
    let mut params = vec![
        Value::from(&customer_id),
        Value::from(settle_period),
        Value::Text(from.to_string()),
        Value::Text(to.map(|day| day.to_string()).unwrap_or_default()),
        Value::Text(to.map(|day| day.to_string()).unwrap_or_default()),
    ];
    if let Some(id) = exclude_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    sql.push_str(" ORDER BY effective_from ASC, policy_no ASC LIMIT 1");
    let rows = ex.query_sql(sql, params).await?;
    rows.first()
        .map(|row| {
            Ok::<PolicyConflict, DbError>(PolicyConflict {
                id: row.get_typed("id")?,
                policy_no: row.get_typed("policy_no")?,
                policy_name: row.get_typed("policy_name")?,
                effective_from: row.get_typed("effective_from")?,
                effective_to: row.get_typed("effective_to")?,
            })
        })
        .transpose()
}

// ==================== 写入 ====================

/// 建政策：**取号 → 政策头 → 规则**，调用方负责事务边界（并已取客户锁 + 查过重叠）。
///
/// 返回 `(新政策 id, 政策号)`。
pub async fn create_policy(
    ex: &impl SqlExecutor,
    new: &NewPolicy<'_>,
    rules: &[RuleWrite],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let policy_no = next_policy_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, policy_no, policy_name, customer_id, rule_type, rate_unit, settle_period, \
                 effective_from, effective_to, status, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, ?::uuid, ?, ?, ?, ?::text::date, NULLIF(?::text,'')::date, \
                 ?::text::smallint, NULLIF(?::text,''), ?, ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(policy_no.clone()),
            Value::from(new.policy_name.to_string()),
            Value::from(&new.customer_id),
            Value::from(new.rule_type.to_string()),
            Value::from(new.rate_unit.to_string()),
            Value::from(new.settle_period.to_string()),
            Value::Text(new.effective_from.to_string()),
            Value::Text(new.effective_to.map(|day| day.to_string()).unwrap_or_default()),
            Value::Text(new.status.to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_rules(ex, id, rules).await?;
    Ok((id, policy_no))
}

/// 改政策：**覆盖政策头 + 规则整表替换**（调用方负责事务、锁与重叠检查）。
///
/// 返回影响行数（0 = 政策在读取与写入之间被删/被改走，调用方按 404 / 409 处理）。
/// `affected = 0` 时**不动规则**（否则会把别人的规则删掉）。
pub async fn update_policy(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewPolicy<'_>,
    rules: &[RuleWrite],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET policy_name = ?, customer_id = ?::uuid, rule_type = ?, rate_unit = ?, \
                        settle_period = ?, effective_from = ?::text::date, \
                        effective_to = NULLIF(?::text,'')::date, status = ?::text::smallint, \
                        remark = NULLIF(?::text,''), updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![
                Value::from(new.policy_name.to_string()),
                Value::from(&new.customer_id),
                Value::from(new.rule_type.to_string()),
                Value::from(new.rate_unit.to_string()),
                Value::from(new.settle_period.to_string()),
                Value::Text(new.effective_from.to_string()),
                Value::Text(new.effective_to.map(|day| day.to_string()).unwrap_or_default()),
                Value::Text(new.status.to_string()),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text,
                Value::from(&id),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    ex.exec_sql(
        format!("DELETE FROM {RULE_TABLE} WHERE policy_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_rules(ex, id, rules).await?;
    Ok(affected)
}

/// 插规则（调用方已按 `seq` 排好序；`seq` 由 `rebate_dto::checked_rules` 重新编号）。
async fn insert_rules(ex: &impl SqlExecutor, policy_id: Uuid, rules: &[RuleWrite]) -> DbResult<()> {
    for rule in rules {
        ex.exec_sql(
            format!(
                "INSERT INTO {RULE_TABLE} \
                    (id, policy_id, seq, tier_from, tier_to, rate, remark, created_at, updated_at) \
                 VALUES (?, ?::uuid, ?::text::smallint, ?::text::numeric, \
                    NULLIF(?::text,'')::numeric, ?::text::numeric, NULLIF(?::text,''), now(), now())"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&policy_id),
                Value::Text(rule.seq.to_string()),
                Value::Text(rule.tier_from.to_string()),
                Value::Text(rule.tier_to.map(|value| value.to_string()).unwrap_or_default()),
                Value::Text(rule.rate.to_string()),
                Value::Text(rule.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 软删政策（规则随政策一起从业务上消失；物理行留着供审计）。
pub async fn soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        &format!(
            "UPDATE {TABLE} SET deleted_at = now(), updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(operator.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 分页锚点必须是「不参与数据权限」的（政策是全局配置表）。
    #[test]
    fn policy_target_is_unscoped() {
        assert_eq!(POLICY_TARGET.table, "biz_sales_rebate_policy");
        assert!(!POLICY_TARGET.is_scoped(), "返利政策是全局配置：不参与数据范围注入");
        println!("[M3-15] 分页锚点 biz_sales_rebate_policy 为 unscoped ✓");
    }

    /// 单号格式与解析是一对（写一个错位的格式，M3-16 按单号回溯就废了）。
    #[test]
    fn policy_no_roundtrip() {
        let no = format_policy_no("20261117", 12);
        assert_eq!(no, "BR-20261117-000012");
        assert_eq!(parse_policy_no(&no), Some(("20261117".to_string(), 12)));
        for bad in [
            "",
            "BR",
            "BR-20261117",
            "QT-20261117-000001",
            "BR-2026-11-17-1",
            "BR-20261117-000000",
            "BR-20261317-000001",
        ] {
            assert_eq!(parse_policy_no(bad), None, "非法单号必须解析失败：{bad}");
        }
        println!("[M3-15] 单号 BR-yyyyMMdd-NNNNNN 组装/解析互逆，7 种非法形状全拒 ✓");
    }

    /// 排序必须带 id 收全序（否则翻页会换行）。
    #[test]
    fn page_order_is_total() {
        assert!(POLICY_ORDER.contains("p.effective_from DESC"));
        assert!(POLICY_ORDER.ends_with("p.id ASC"), "必须以 id 收全序：{POLICY_ORDER}");
        println!("[M3-15] 列表全序：生效日倒序 → 政策号 → id ✓");
    }

    /// 「今天是否生效」的边界：首末日都含、长期有效恒真、停用恒假。
    #[test]
    fn active_on_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 17).expect("合法日期");
        let row = |from: NaiveDate, to: Option<NaiveDate>, status: i16| RebatePolicyRow {
            id: Uuid::nil(),
            policy_no: String::new(),
            policy_name: String::new(),
            customer_id: Uuid::nil(),
            customer_code: None,
            customer_name: None,
            rule_type: "BY_QTY".to_string(),
            rate_unit: "PER_UNIT".to_string(),
            settle_period: "MONTH".to_string(),
            effective_from: from,
            effective_to: to,
            status,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).expect("合法日期");
        assert!(
            row(day(17), Some(day(17)), 1).is_active_on(today),
            "当天开始当天结束也算生效"
        );
        assert!(row(day(1), None, 1).is_active_on(today), "长期有效");
        assert!(!row(day(18), None, 1).is_active_on(today), "还没开始");
        assert!(!row(day(1), Some(day(16)), 1).is_active_on(today), "已失效");
        assert!(!row(day(1), None, 0).is_active_on(today), "停用恒假");
        println!("[M3-15] 生效判定：起止日都含 / 长期 / 未开始 / 已失效 / 停用 ✓");
    }
}
