//! 预算守卫（ARCH-6 第三章）：**两道闸的纯函数判定** + 占用事实的写入 / 释放。
//!
//! # 口径（逐条对应 ARCH-6 §3.1）
//!
//! ```text
//! 年可用 = 年度额度(槽) − Σ有效 usage(槽，本年度)        -- OCCUPIED + CONSUMED
//! 月可用 = 月度额度(槽，期间 P) − Σ有效 usage(槽，期间 P)
//! 超支额 = max(0, M − 年可用, M − 月可用)
//! over   = (M > 年可用) OR (M > 月可用)
//! ```
//!
//! * **占用中 / 已执行** 都算「已用」：钱已经承诺出去了（`usage.status IN ('OCCUPIED','CONSUMED')`）；
//!   `RELEASED` 不算（单据被驳回 / 撤回 / 取消，额度还回来了）。
//! * **可用余额绝不缓存**（本章红线 2）：所有额度都是现算，库里没有 `used_amount` 这类列，
//!   `biz_budget_usage.budget_id` 只是「当时命中的版本」审计快照，**不参与计算**（D5）。
//! * **`M = 可用` 不算超**（边界用例，见单测 `boundary_equal_is_not_over`）。
//!
//! # 超预算怎么办：策略 B（标记 + 强制审批），409 只在显式 BLOCK
//!
//! | mode | 行为 |
//! |---|---|
//! | `OFF` | 不校验、不占用（只记 INFO 日志） |
//! | `WARN`（**出厂默认**） | 照常提交，`usage.over_budget=1` + `over_amount` 冻结当时的超支额，审批单 `form_data.over_budget=true` |
//! | `BLOCK` | 提交直接 409（`ERP_BUDGET_EXCEEDED`），调用方回滚事务 —— 连 usage 都不留 |
//!
//! 为什么默认 WARN：忘编预算 / 编漏一个科目不该让业务停摆；标记 + 强制审批让人来兜
//! （ARCH-6 D3）。BLOCK 必须由客户显式打开（槽级 `biz_budget.control_mode` 或全局
//! `sys_config erp.budget.control_mode`）。
//!
//! # 并发（不锁，靠唯一键 + 快照，ARCH-6 §3.6）
//!
//! 两张单同时提交都超预算 → 两张都带标记进审批（可接受：人都能看见）；
//! 同一张单双击 → 第二次撞 `uk_biz_budget_usage_source`，调用方把 23505 当**幂等成功**；
//! `BLOCK` 模式若真要做到「一分不超」，才需要 `pg_advisory_xact_lock(hashtext(dept||account||year))`
//! —— 本设计**预留但不实现**（§3.6.2 已论证代价）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 全局控制策略的 `sys_config` 键。
pub const CONFIG_CONTROL_MODE: &str = "erp.budget.control_mode";
/// 采购提交是否做预算校验的 `sys_config` 键。
pub const CONFIG_ENFORCE_ON_PO: &str = "erp.budget.enforce_on_po";

/// 占用来源：费用报销单（FIN-3 接线）。
pub const SOURCE_EXPENSE: &str = "EXPENSE_REPORT";
/// 占用来源：采购订单（本卡接线）。
pub const SOURCE_PURCHASE: &str = "PURCHASE_ORDER";

/// usage 状态：占用中。
pub const USAGE_OCCUPIED: &str = "OCCUPIED";
/// usage 状态：已执行（已入账 / 已收货）。
pub const USAGE_CONSUMED: &str = "CONSUMED";
/// usage 状态：已释放（驳回 / 撤回 / 取消 / 作废）。
pub const USAGE_RELEASED: &str = "RELEASED";

/// 预算控制策略（`biz_budget.control_mode` / `sys_config` 的取值域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlMode {
    /// 不校验、不占用。
    Off,
    /// 标记超预算 + 强制审批（出厂默认）。
    Warn,
    /// 直接阻断（409）。
    Block,
}

impl ControlMode {
    /// 全部取值（顺序 = 严格程度递增）。
    pub const ALL: [ControlMode; 3] = [ControlMode::Off, ControlMode::Warn, ControlMode::Block];

    /// 落库值。
    pub const fn as_str(self) -> &'static str {
        match self {
            ControlMode::Off => "OFF",
            ControlMode::Warn => "WARN",
            ControlMode::Block => "BLOCK",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            ControlMode::Off => "不校验",
            ControlMode::Warn => "标记并强制审批",
            ControlMode::Block => "阻断提交",
        }
    }

    /// 字符串 → 策略。**未知一律 `None`**（调用方报错或按默认处理，不许静默猜测）。
    pub fn parse(raw: &str) -> Option<ControlMode> {
        ControlMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == raw.trim().to_ascii_uppercase())
    }

    /// 槽级 `control_mode` 为空时继承全局（ARCH-6 §3.2.1 的 `NULL` 语义）。
    pub fn resolve(slot_mode: Option<&str>, global_mode: ControlMode) -> ControlMode {
        slot_mode
            .and_then(ControlMode::parse)
            .unwrap_or(global_mode)
    }

    /// 是否「真的校验」（`OFF` 不校验也不占用）。
    pub const fn enforces(self) -> bool {
        !matches!(self, ControlMode::Off)
    }
}

/// 一个预算「槽」= 部门 × 预算科目 × 年度（D5：usage 绑槽不绑版本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// 部门 id（采购侧取提交人部门快照）。
    pub dept_id: Uuid,
    /// 预算科目 id（末级）。
    pub account_id: Uuid,
    /// 会计年度。
    pub fiscal_year: i16,
    /// 发生期间 `yyyy-MM`（含月份，月度闸用）。
    pub period_code: String,
}

impl Slot {
    /// 构造（`fiscal_year` 从期间码推导，避免两处手填不一致）。
    pub fn new(dept_id: Uuid, account_id: Uuid, period_code: &str) -> Option<Self> {
        let year: i16 = period_code.get(0..4)?.parse().ok()?;
        let month: u32 = period_code.get(5..7)?.parse().ok()?;
        if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        Some(Self {
            dept_id,
            account_id,
            fiscal_year: year,
            period_code: period_code.to_string(),
        })
    }

    /// 槽的年度键（日志 / 文案用）。
    pub fn year_text(&self) -> String {
        format!("{} 年", self.fiscal_year)
    }
}

/// 判定结果（**纯数据，不碰库**，六个数全给，便于日志与单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetVerdict {
    /// 生效的控制策略。
    pub mode: ControlMode,
    /// 年度额度（无 ACTIVE 预算 = 0）。
    pub year_limit: Decimal,
    /// 年度已用（OCCUPIED + CONSUMED）。
    pub year_used: Decimal,
    /// 月度额度（无分解行 = 0）。
    pub month_limit: Decimal,
    /// 月度已用（同期间）。
    pub month_used: Decimal,
    /// 本次提交金额。
    pub this_amount: Decimal,
    /// 年度缺口 = max(0, 本次 − 年可用)。
    pub year_gap: Decimal,
    /// 月度缺口 = max(0, 本次 − 月可用)。
    pub month_gap: Decimal,
    /// 是否超预算（任一缺口 > 0）。
    pub over: bool,
    /// 该槽根本没有 ACTIVE 预算（额度按 0 处理）。
    pub missing_budget: bool,
}

impl BudgetVerdict {
    /// 判定（纯函数）。**`this == 可用` 不算超**（边界不能靠 `>=` 的直觉）。
    pub fn evaluate(
        mode: ControlMode,
        year_limit: Decimal,
        year_used: Decimal,
        month_limit: Decimal,
        month_used: Decimal,
        this_amount: Decimal,
    ) -> Self {
        let year_avail = (year_limit - year_used).max(Decimal::ZERO);
        let month_avail = (month_limit - month_used).max(Decimal::ZERO);
        let year_gap = (this_amount - year_avail).max(Decimal::ZERO);
        let month_gap = (this_amount - month_avail).max(Decimal::ZERO);
        Self {
            mode,
            year_limit,
            year_used,
            month_limit,
            month_used,
            this_amount,
            year_gap,
            month_gap,
            over: year_gap > Decimal::ZERO || month_gap > Decimal::ZERO,
            missing_budget: year_limit <= Decimal::ZERO && year_used <= Decimal::ZERO,
        }
    }

    /// 超支额 = 两个缺口里更大的那个（报文与快照都用它）。
    pub fn over_amount(&self) -> Decimal {
        self.year_gap.max(self.month_gap)
    }

    /// 年可用（额度 − 已用，负数截零）。
    pub fn year_available(&self) -> Decimal {
        (self.year_limit - self.year_used).max(Decimal::ZERO)
    }

    /// 月可用。
    pub fn month_available(&self) -> Decimal {
        (self.month_limit - self.month_used).max(Decimal::ZERO)
    }

    /// 是否应该阻断（`BLOCK` 且真的超了）。
    pub fn blocks(&self) -> bool {
        self.mode == ControlMode::Block && self.over
    }

    /// 一句话结论（日志 / 409 报文 / 审批备注共用，避免三处各写一套文案）。
    pub fn describe(&self) -> String {
        let head = if self.missing_budget {
            "未编制预算".to_string()
        } else if self.over {
            format!("超出预算（超支 {}）", self.over_amount())
        } else {
            format!(
                "预算内（剩余 {}）",
                self.year_available().min(self.month_available())
            )
        };
        format!(
            "{head}：本次 {} ｜ 年度 额度 {} / 已用 {} / 可用 {} ｜ 月度 额度 {} / 已用 {} / 可用 {} ｜ 策略 {}（{}）",
            self.this_amount,
            self.year_limit,
            self.year_used,
            self.year_available(),
            self.month_limit,
            self.month_used,
            self.month_available(),
            self.mode.as_str(),
            self.mode.label()
        )
    }
}

/// 一个槽的现状（查库结果，喂给 [`BudgetVerdict::evaluate`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSnapshot {
    /// 最新 ACTIVE 版本的 id（`None` = 该槽没有生效预算）。
    pub budget_id: Option<Uuid>,
    /// 年度额度。
    pub year_limit: Decimal,
    /// 年度已用。
    pub year_used: Decimal,
    /// 月度额度。
    pub month_limit: Decimal,
    /// 月度已用。
    pub month_used: Decimal,
}

/// 全局控制策略（`sys_config`，缺省 = `WARN`）。
pub async fn global_mode(ex: &impl SqlExecutor) -> DbResult<ControlMode> {
    // SCOPE-EXEMPT: 全局配置读 1 行，无归属列。
    let rows = ex
        .query_sql(
            "SELECT config_value FROM sys_config WHERE config_key = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(CONFIG_CONTROL_MODE.to_string())],
        )
        .await?;
    let raw: Option<String> = match rows.first() {
        None => None,
        Some(row) => Some(row.get_typed("config_value")?),
    };
    Ok(raw
        .as_deref()
        .and_then(ControlMode::parse)
        .unwrap_or(ControlMode::Warn))
}

/// 采购提交是否做预算校验（`erp.budget.enforce_on_po`，缺省 true）。
pub async fn enforce_on_po(ex: &impl SqlExecutor) -> DbResult<bool> {
    // SCOPE-EXEMPT: 同上，全局开关 1 行。
    let rows = ex
        .query_sql(
            "SELECT config_value FROM sys_config WHERE config_key = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(CONFIG_ENFORCE_ON_PO.to_string())],
        )
        .await?;
    let raw: Option<String> = match rows.first() {
        None => None,
        Some(row) => Some(row.get_typed("config_value")?),
    };
    Ok(!matches!(
        raw.as_deref().map(str::trim),
        Some("false") | Some("0") | Some("FALSE")
    ))
}

/// 读一个槽的现状（现算，不缓存）。
///
/// 三条聚合都走 `idx_biz_budget_usage_slot*`（ARCH-6 红线 2 的实测依据）。
pub async fn slot_snapshot(ex: &impl SqlExecutor, slot: &Slot) -> DbResult<SlotSnapshot> {
    // ① 最新 ACTIVE 版本（槽级唯一索引保证最多 1 行）
    let active = ex
        .query_sql(
            "SELECT id, total_amount::text AS total_amount \
               FROM biz_budget \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status = 'ACTIVE' AND deleted_at IS NULL \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
            ],
        )
        .await?;
    let (budget_id, year_limit) = match active.first() {
        None => (None, Decimal::ZERO),
        Some(row) => {
            let id: Uuid = row.get_typed("id")?;
            let text: String = row.get_typed("total_amount")?;
            (Some(id), parse_amount(&text))
        }
    };
    // ② 年度已用 / 月度已用（同槽、有效状态）
    let used = ex
        .query_sql(
            "SELECT COALESCE(SUM(amount), 0)::text AS year_used, \
                    COALESCE(SUM(amount) FILTER (WHERE period_code = ?), 0)::text AS month_used \
               FROM biz_budget_usage \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(slot.period_code.clone()),
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
            ],
        )
        .await?;
    let (year_used, month_used) = match used.first() {
        None => (Decimal::ZERO, Decimal::ZERO),
        Some(row) => {
            let y: String = row.get_typed("year_used")?;
            let m: String = row.get_typed("month_used")?;
            (parse_amount(&y), parse_amount(&m))
        }
    };
    // ③ 月度额度（分解行；没有 ACTIVE 版本时必然 0）
    let month_limit = match budget_id {
        None => Decimal::ZERO,
        Some(id) => {
            let rows = ex
                .query_sql(
                    "SELECT amount::text AS amount FROM biz_budget_period \
                      WHERE budget_id = ?::uuid AND period_code = ?"
                        .to_string(),
                    vec![Value::from(&id), Value::Text(slot.period_code.clone())],
                )
                .await?;
            match rows.first() {
                None => Decimal::ZERO,
                Some(row) => {
                    let text: String = row.get_typed("amount")?;
                    parse_amount(&text)
                }
            }
        }
    };
    Ok(SlotSnapshot {
        budget_id,
        year_limit,
        year_used,
        month_limit,
        month_used,
    })
}

/// 读槽 + 判定（一次查库 + 纯函数）。
pub async fn check_slot(
    ex: &impl SqlExecutor,
    slot: &Slot,
    amount: Decimal,
    slot_mode: Option<&str>,
) -> DbResult<BudgetVerdict> {
    let global = global_mode(ex).await?;
    let mode = ControlMode::resolve(slot_mode, global);
    let snapshot = slot_snapshot(ex, slot).await?;
    Ok(BudgetVerdict::evaluate(
        mode,
        snapshot.year_limit,
        snapshot.year_used,
        snapshot.month_limit,
        snapshot.month_used,
        amount,
    ))
}

/// 写一条占用（`OCCUPIED`）。**与来源单据的状态流转同事务**（ARCH-6 §3.5.1 事务边界）。
///
/// * `verdict` 的 `over_budget / over_amount` 作为**提交时点快照**落库（额度后来被调高，
///   历史单据的「当时超没超」不许跟着变）；
/// * `budget_id` 只写审计列（D5：不参与计算）；
/// * 撞 `uk_biz_budget_usage_source`（重复提交 / 重试）→ 返回 `None`，调用方当**幂等成功**。
#[allow(clippy::too_many_arguments)]
pub async fn occupy(
    ex: &impl SqlExecutor,
    slot: &Slot,
    amount: Decimal,
    amount_net: Decimal,
    source_type: &str,
    source_id: Uuid,
    source_no: &str,
    verdict: &BudgetVerdict,
    snapshot: &SlotSnapshot,
    operator: Option<Uuid>,
) -> DbResult<Option<Uuid>> {
    let id = Uuid::now_v7();
    let result = ex
        .exec_sql(
            "INSERT INTO biz_budget_usage (
                 id, dept_id, account_id, fiscal_year, period_code,
                 amount, amount_net, source_type, source_id, source_no,
                 status, budget_id, over_budget, over_amount, create_by)
             VALUES (?, ?::uuid, ?::uuid, ?::int, ?, ?::text::numeric, ?::text::numeric,
                     ?, ?::uuid, ?, 'OCCUPIED', NULLIF(?::text,'')::uuid, ?::int, ?::text::numeric,
                     NULLIF(?::text,'')::uuid)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
                Value::Text(slot.period_code.clone()),
                Value::Text(amount.to_string()),
                Value::Text(amount_net.to_string()),
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
                Value::Text(source_no.to_string()),
                Value::Text(
                    snapshot
                        .budget_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(i32::from(verdict.over)),
                Value::Text(verdict.over_amount().to_string()),
                Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            ],
        )
        .await;
    match result {
        Ok(_) => Ok(Some(id)),
        Err(DbError::UniqueViolation { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// 释放某张单据的全部占用（驳回 / 撤回 / 取消 / 作废）。**只改状态，不删行**（留痕）。
pub async fn release(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    reason: &str,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget_usage \
            SET status = 'RELEASED', released_at = now(), release_reason = ?, updated_at = now() \
          WHERE source_type = ? AND source_id = ?::uuid \
            AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(truncate(reason, 255)),
            Value::Text(source_type.to_string()),
            Value::from(&source_id),
        ],
    )
    .await
}

/// 把某张单据的占用转成「已执行」（报销入账 / 采购订单关闭）。
pub async fn consume(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    voucher_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget_usage \
            SET status = 'CONSUMED', consumed_at = now(), voucher_id = NULLIF(?::text,'')::uuid, updated_at = now() \
          WHERE source_type = ? AND source_id = ?::uuid \
            AND status = 'OCCUPIED' AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(voucher_id.map(|id| id.to_string()).unwrap_or_default()),
            Value::Text(source_type.to_string()),
            Value::from(&source_id),
        ],
    )
    .await
}

/// 某张单据当前的占用快照（详情页 / 测试用）。
pub async fn usages_of(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Vec<UsageRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, dept_id, account_id, fiscal_year, period_code, amount::text AS amount, \
                    status, (over_budget <> 0) AS over_budget, over_amount::text AS over_amount, \
                    source_no, released_at, release_reason \
               FROM biz_budget_usage \
              WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY created_at"
                .to_string(),
            vec![
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(UsageRow {
                id: row.get_typed("id")?,
                dept_id: row.get_typed("dept_id")?,
                account_id: row.get_typed("account_id")?,
                fiscal_year: row.get_typed("fiscal_year")?,
                period_code: row.get_typed("period_code")?,
                amount: parse_amount(&row.get_typed::<String>("amount")?),
                status: row.get_typed("status")?,
                over_budget: row.get_typed("over_budget")?,
                over_amount: parse_amount(&row.get_typed::<String>("over_amount")?),
                source_no: row.get_typed("source_no")?,
                released_at: row.get_typed("released_at")?,
                release_reason: row.get_typed("release_reason")?,
            })
        })
        .collect()
}

/// 一条占用记录（读模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    /// 主键。
    pub id: Uuid,
    /// 槽：部门。
    pub dept_id: Uuid,
    /// 槽：科目。
    pub account_id: Uuid,
    /// 槽：年度。
    pub fiscal_year: i16,
    /// 发生期间。
    pub period_code: String,
    /// 占用金额（含税）。
    pub amount: Decimal,
    /// OCCUPIED / CONSUMED / RELEASED。
    pub status: String,
    /// 提交时点是否超预算。
    pub over_budget: bool,
    /// 提交时点超支额。
    pub over_amount: Decimal,
    /// 来源单号。
    pub source_no: String,
    /// 释放时间。
    pub released_at: Option<DateTime<Utc>>,
    /// 释放原因。
    pub release_reason: Option<String>,
}

fn parse_amount(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ==================== 采购侧接线（控制点 ②，ARCH-6 §3.5.2） ====================

/// 采购提交时的预算判定结果（含「解析不到科目」这个分支）。
///
/// `verdict: None` 有两种含义，靠 [`PurchaseBudget::reason`] 区分：
/// * 该槽没有映射到预算科目（`mapped = false`）；
/// * 控制策略是 `OFF`（不校验、不占用）。
#[derive(Debug, Clone)]
pub struct PurchaseBudget {
    /// 生效策略。
    pub mode: ControlMode,
    /// 是否解析到了预算科目。
    pub mapped: bool,
    /// 命中的映射类型（MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT）。
    pub map_type: Option<String>,
    /// 预算科目 id。
    pub account_id: Option<Uuid>,
    /// 预算槽（期间取单据日期）。
    pub slot: Option<Slot>,
    /// 判定结果（`None` = 没校验）。
    pub verdict: Option<BudgetVerdict>,
    /// 一句话说明（日志 / 报文用）。
    pub reason: String,
}

impl PurchaseBudget {
    /// 是否超预算。
    pub fn over(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.over)
    }

    /// 是否应该 409 阻断（`BLOCK` + 超预算）。
    pub fn blocks(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.blocks())
    }

    /// 是否需要写占用（解析到科目 + 策略不是 OFF）。
    pub fn should_occupy(&self) -> bool {
        self.mapped && self.mode.enforces()
    }
}

/// 采购单提交时的预算判定（**只读**，不写 usage）。
///
/// 科目解析顺序见 [`crate::business::erp::budget_model::resolve_for_po`]；
/// 部门取「提交人部门」快照（`voucher::user_dept_of`），结算/期间取单据日期。
/// 解析不到科目时按 ARCH-6 §3.2.5 的口径处置：
/// `OFF` / `WARN` → **不占用 + 告警**（映射是运维配置，配不全不该让采购停摆）；
/// `BLOCK` → 调用方 409（强管控下「配置缺失」是硬错误）。
pub async fn check_for_purchase(
    ex: &impl SqlExecutor,
    order: &crate::business::erp::model::PurchaseOrderRow,
    operator_dept: Option<Uuid>,
) -> DbResult<PurchaseBudget> {
    let global = global_mode(ex).await?;
    // 采购侧的全局开关：`erp.budget.enforce_on_po = false` → 整条链路退化成 OFF
    if !enforce_on_po(ex).await? {
        return Ok(PurchaseBudget {
            mode: ControlMode::Off,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: "erp.budget.enforce_on_po = false：采购提交不做预算校验、不占用预算"
                .to_string(),
        });
    }
    // 部门：提交人部门优先，缺失时退回制单人部门
    let dept_id = match operator_dept {
        Some(dept) => Some(dept),
        None => match order.create_by {
            Some(creator) => crate::business::erp::voucher::user_dept_of(ex, creator).await?,
            None => None,
        },
    };
    let Some(dept_id) = dept_id else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason:
                "提交人 / 制单人都没有部门：无法定位预算槽，本单不占用预算（请在用户资料里补部门）"
                    .to_string(),
        });
    };
    // 物料：取明细第一行（多物料按金额占比拆是 v2 的事，ARCH-6 §3.2.5 的 ① 命中即可）
    let items = crate::business::erp::purchase_model::items_of(order.id).await?;
    let material_id = items.first().map(|item| item.material_id);
    let material_type = match material_id {
        Some(material) => material_type_code_of(ex, material).await?,
        None => None,
    };
    let resolved = crate::business::erp::budget_model::resolve_for_po(
        ex,
        Some(dept_id),
        Some(order.warehouse_id),
        material_id,
        material_type.as_deref(),
    )
    .await?;
    let Some((account_id, map_type)) = resolved else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: format!(
                "采购单未匹配到预算科目（部门 {} / 仓库 {}）：本单不计入预算；\
                 请管理员在「预算科目映射」里配置（物料 / 物料类型 / 仓库 / 默认兜底）",
                dept_id, order.warehouse_id
            ),
        });
    };
    let period_code = order.order_date.format("%Y-%m").to_string();
    let Some(slot) = Slot::new(dept_id, account_id, &period_code) else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: Some(map_type),
            account_id: Some(account_id),
            slot: None,
            verdict: None,
            reason: format!("单据日期 {period_code} 不是合法期间，本单不占用预算"),
        });
    };
    // 槽级策略来自该槽的 ACTIVE 预算（没编预算 → 继承全局）
    let slot_mode = active_control_mode(ex, &slot).await?;
    let mode = ControlMode::resolve(slot_mode.as_deref(), global);
    let snapshot = slot_snapshot(ex, &slot).await?;
    let verdict = BudgetVerdict::evaluate(
        mode,
        snapshot.year_limit,
        snapshot.year_used,
        snapshot.month_limit,
        snapshot.month_used,
        order.total_with_tax,
    );
    Ok(PurchaseBudget {
        mode,
        mapped: true,
        map_type: Some(map_type),
        account_id: Some(account_id),
        slot: Some(slot),
        reason: verdict.describe(),
        verdict: Some(verdict),
    })
}

/// 该槽 ACTIVE 预算的控制策略（`None` = 没预算 / 没配 → 继承全局）。
pub async fn active_control_mode(ex: &impl SqlExecutor, slot: &Slot) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 槽级精确查询，返回 0..1 行的单列。
    let rows = ex
        .query_sql(
            "SELECT control_mode FROM biz_budget \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status = 'ACTIVE' AND deleted_at IS NULL LIMIT 1"
                .to_string(),
            vec![
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("control_mode"),
    }
}

/// 物料的类型编码（`biz_material.material_type`）—— 供 MATERIAL_TYPE 映射命中。
async fn material_type_code_of(
    ex: &impl SqlExecutor,
    material_id: Uuid,
) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取 1 行的单列。
    let rows = ex
        .query_sql(
            "SELECT material_type FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("material_type"),
    }
}

/// 写采购单的占用（**与单据状态流转同事务**，ARCH-6 §3.5.1 事务边界）。
///
/// 返回 `Some(usage_id)` = 新写入；`None` = 未占用（没解析到科目 / 策略 OFF /
/// 撞唯一键的幂等重放）。
pub async fn occupy_for_purchase(
    ex: &impl SqlExecutor,
    order: &crate::business::erp::model::PurchaseOrderRow,
    check: &PurchaseBudget,
    operator: Option<Uuid>,
) -> DbResult<Option<Uuid>> {
    if !check.should_occupy() {
        return Ok(None);
    }
    let (Some(slot), Some(verdict)) = (&check.slot, &check.verdict) else {
        return Ok(None);
    };
    let snapshot = slot_snapshot(ex, slot).await?;
    occupy(
        ex,
        slot,
        order.total_with_tax,
        order.total_amount,
        SOURCE_PURCHASE,
        order.id,
        &order.order_no,
        verdict,
        &snapshot,
        operator,
    )
    .await
}

// ==================== 报销侧接线（控制点 ①，ARCH-6 §3.5.1） ====================
//
// 与采购侧（控制点 ②）的差别：
// * 采购是「一个物料 → 一个预算科目」（`resolve_for_po` 按物料 / 类型 / 仓库 / 默认兜底解析）；
// * 报销是「**明细自带科目**」—— 每条明细在提交时就把 `account_id` 快照下来了
//   （`biz_expense_item.account_id`），所以只要按 `(部门, 科目)` 聚合就得到槽金额，
//   不需要再解析映射表（ARCH-6 §3.5.1 的「按明细科目聚合写 N 行（同科目合并）」）。
// * `budget_controlled = 0` 的费用类型（罚款 / 滞纳金）**不产生 usage、不参与超支判定**
//   （ARCH-6 §4.6 末段）—— 由调用方在传参前过滤掉。

/// 一条待判定的「槽金额」：同一科目的明细已由调用方聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpenseSlotAmount {
    /// 预算科目（明细快照的借方科目）。
    pub account_id: Uuid,
    /// 含税金额（预算口径：报销按价税合计占额度）。
    pub amount: Decimal,
    /// 不含税金额（`amount_net` 列，只做分析，不参与判定）。
    pub amount_net: Decimal,
}

/// 一个槽的判定结果（含快照，供 `occupy_for_expense` 写 `budget_id`）。
#[derive(Debug, Clone)]
pub struct ExpenseSlotCheck {
    /// 槽。
    pub slot: Slot,
    /// 含税金额。
    pub amount: Decimal,
    /// 不含税金额。
    pub amount_net: Decimal,
    /// 判定结果。
    pub verdict: BudgetVerdict,
    /// 快照（`budget_id` 写进 usage 作审计）。
    pub snapshot: SlotSnapshot,
}

/// 报销提交时的预算判定汇总。
#[derive(Debug, Clone)]
pub struct ExpenseBudget {
    /// 生效的全局策略（每个槽可能被槽级 `control_mode` 覆盖）。
    pub mode: ControlMode,
    /// 部门。
    pub dept_id: Uuid,
    /// 期间（提交时按费用发生日所属期间取；入账时再按期重算不影响 usage，
    /// 因为 usage 的槽是按提交时期间冻结的审计事实）。
    pub period_code: String,
    /// 逐槽结果（同科目已合并）。
    pub checks: Vec<ExpenseSlotCheck>,
    /// 一句话说明（日志 / 报文用）。
    pub reason: String,
}

impl ExpenseBudget {
    /// 是否有任一槽超预算。
    pub fn over(&self) -> bool {
        self.checks.iter().any(|check| check.verdict.over)
    }

    /// 是否应该 409 阻断（`BLOCK` + 超预算）。
    pub fn blocks(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.verdict.blocks() && check.verdict.over)
    }

    /// 超支额（多槽取最大；与 `form_data.over_amount` 同口径）。
    pub fn over_amount(&self) -> Decimal {
        self.checks
            .iter()
            .filter(|check| check.verdict.over)
            .map(|check| check.verdict.over_amount())
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// 是否需要写占用（`OFF` 策略不写）。
    pub fn should_occupy(&self) -> bool {
        self.mode.enforces() && !self.checks.is_empty()
    }
}

/// 报销提交时的预算判定（**只读**，不写 usage）。
///
/// `dept_id` 取报销单上的部门快照（不是「提交人当前部门」—— 单据是部门维度的凭证，
/// 报销人换了部门不该让历史单据的预算归属漂移）。
pub async fn check_for_expense(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    period_code: &str,
    items: &[ExpenseSlotAmount],
) -> DbResult<ExpenseBudget> {
    let global = global_mode(ex).await?;
    if !global.enforces() || items.is_empty() {
        return Ok(ExpenseBudget {
            mode: global,
            dept_id,
            period_code: period_code.to_string(),
            checks: Vec::new(),
            reason: if items.is_empty() {
                "本单没有受预算控制的明细（费用类型 budget_controlled = 0）：不占用预算".to_string()
            } else {
                "erp.budget.control_mode = OFF：报销提交不做预算校验、不占用预算".to_string()
            },
        });
    }
    let mut checks = Vec::with_capacity(items.len());
    for item in items {
        let Some(slot) = Slot::new(dept_id, item.account_id, period_code) else {
            return Ok(ExpenseBudget {
                mode: global,
                dept_id,
                period_code: period_code.to_string(),
                checks: Vec::new(),
                reason: format!("期间 {period_code} 不是合法会计期间：本单不占用预算"),
            });
        };
        let slot_mode = active_control_mode(ex, &slot).await?;
        let mode = ControlMode::resolve(slot_mode.as_deref(), global);
        let snapshot = slot_snapshot(ex, &slot).await?;
        let verdict = BudgetVerdict::evaluate(
            mode,
            snapshot.year_limit,
            snapshot.year_used,
            snapshot.month_limit,
            snapshot.month_used,
            item.amount,
        );
        checks.push(ExpenseSlotCheck {
            slot,
            amount: item.amount,
            amount_net: item.amount_net,
            verdict,
            snapshot,
        });
    }
    let over_slots = checks.iter().filter(|check| check.verdict.over).count();
    let reason = if over_slots == 0 {
        format!("{} 个预算槽均在额度内", checks.len())
    } else {
        format!(
            "{over_slots}/{} 个预算槽超额度，超支额最大 {}（策略 {}）",
            checks.len(),
            checks
                .iter()
                .map(|check| check.verdict.over_amount())
                .max()
                .unwrap_or(Decimal::ZERO),
            global.label()
        )
    };
    Ok(ExpenseBudget {
        mode: global,
        dept_id,
        period_code: period_code.to_string(),
        checks,
        reason,
    })
}

/// 写报销单的占用行（`OCCUPIED`）。**与「状态 → SUBMITTED」同事务**。
///
/// 返回实际写入的行数；重复提交（撞 `uk_biz_budget_usage_source`）按幂等成功处理
/// （`occupy` 返回 `None`），不算错。
pub async fn occupy_for_expense(
    ex: &impl SqlExecutor,
    budget: &ExpenseBudget,
    source_id: Uuid,
    source_no: &str,
    operator: Option<Uuid>,
) -> DbResult<usize> {
    if !budget.should_occupy() {
        return Ok(0);
    }
    let mut written = 0;
    for check in &budget.checks {
        if !check.verdict.mode.enforces() {
            continue;
        }
        let id = occupy(
            ex,
            &check.slot,
            check.amount,
            check.amount_net,
            SOURCE_EXPENSE,
            source_id,
            source_no,
            &check.verdict,
            &check.snapshot,
            operator,
        )
        .await?;
        if id.is_some() {
            written += 1;
        }
    }
    Ok(written)
}

// ==================== 编译期自检（纯函数，不连库） ====================

#[cfg(test)]
#[path = "budget_guard_tests.rs"]
mod budget_guard_tests;
