//! 结算 ②「三张凭证」：人工计提 / 制造费用结转 / 完工入库结转（ARCH-9 §4.7.2–§4.7.5 · COST-B3-5）。
//!
//! ```text
//! ④ §4.8 事务体里的凭证步骤（本文件）—— 全部经 voucher::post_voucher（唯一收口）
//!
//!  ④-a 人工计提（条件）      借 4001.02 生产成本-直接人工   贷 2211.01 应付职工薪酬-工资
//!                            · 按车间（sys_dept）分行、aux_dept_id = 车间
//!                            · labor_source='PAYROLL' ⇒ 不生成（工资凭证由薪资单出，D-13）
//!  ④-b 手工费用台账（按需）  借 4101.xx 制造费用-*         贷 对方科目（credit_account_code）
//!                            · §4.7.3 第二段：台账行先「进 4101」，随后被 ④-c 清零
//!  ④-c 制造费用结转          借 4001.03 生产成本-制造费用   贷 4101.xx（按科目清零）
//!                            · 借按 (车间, 科目) 分行、aux_dept_id = 车间
//!  ④-d 完工入库结转          借 1405 库存商品              贷 4001.01 / .02 / .03（按要素三行）
//!                            · 借按成本对象分行、aux_material_id + aux_warehouse_id
//!                            · 生成后回写 biz_stock_in.voucher_id（§4.7.4 末段）
//! ```
//!
//! # 为什么没有 HTTP 入口
//!
//! 设计 §5.2 的接口清单里没有「发凭证」这一步：凭证是结算事务（#29，B3-6）的内部步骤，
//! 单独暴露会让期间停在「入库了但没凭证」的半算状态 —— 那正是 AGENTS 约定 11 要消灭的形态。
//! 本文件只落语句、不 `commit`，事务边界归调用方（与 `cost_settle_finish` 同款契约）。
//!
//! # 金额口径：一律「来自计算单 / 来自归集行」，绝不重算
//!
//! | 凭证 | 借方来源 | 贷方来源 | 对平判据（本文件内**硬断言**） |
//! |---|---|---|---|
//! | 人工计提 | `biz_cost_collect`（element=LABOR，按车间汇总） | 同额 | Σ借 = Σ`item.labor_cur` |
//! | 制造费用结转 | `biz_cost_collect`（element=OVERHEAD，按 (车间,科目)） | 同额（按科目） | Σ借 = Σ`item.oh_cur` |
//! | 完工结转 | `item.finished_cost`（逐对象） | 逐要素拆分，合计 = `finished_cost` | Σ借 = Σ`item.finished_cost` |
//!
//! 三条断言不平 ⇒ **409 + `66076`**（账账不符，绝不出不平的凭证）。这是卡面验收
//! 「金额与计算单一致」的运行期证据，不是只在单测里成立的漂亮话。
//!
//! # 完工结转的要素拆分（§4.7.5 的贷方三行）
//!
//! 计算单只存 `finished_cost` 与三个 `unit_*`（没有「完工材料 / 完工人工 / 完工费用」三列），
//! 所以贷方逐要素金额 = `round(completed_qty × unit_*, 4)`，再与 `finished_cost` 对齐：
//!
//! ```text
//! residual = finished_cost − (fm + fl + fo)
//! ```
//!
//! `residual ≠ 0` 只会出现在 B2-11 的「期末在制数量 0 ⇒ 尾差进完工」分支（量级 0.0001–0.0008）。
//! 它按 **制造费用 → 材料 → 人工** 的顺序吸收（与 B2-11「期初在制差额落制造费用」同口径：
//! 费用是成本表里的倒挤项），逻辑见 [`split_finished`]，纯函数 + 单测钉死。
//!
//! # ★ 与设计原文的两处偏差（都在卡内报备，见卡内「已知偏差」）
//!
//! 1. **制造费用结转的借方包含「直接行」**。§4.7.3 原文只写 `Σ 池金额`，但 `biz_cost_overhead`
//!    可以挂成本对象（§4.3 费·手工 ⇒ 直接行），那部分费用的 4101 借方同样要靠这张凭证清零 ——
//!    只清池会让 ① 4101 期末余额 ≠ 0（C-COST-1 红）、② `Σ 4001.03 借方 ≠ Σ item.oh_cur`
//!    （C-COST-3 红、本文件的自检 66076 直接拦死）。设计原文的**目的**写得很明确
//!    （「把 4101 清零，让制造费用全部进产品成本」），按目的实现，按原文只做池会自相矛盾。
//! 2. **多一张「手工费用台账」凭证**（§4.7.3 第二段 + §4.8 ④「第 4 张，按需」）。
//!    它不是新设计：手工台账是**唯一**没有上游凭证的费用来源（折旧/报销在上游已借 4101），
//!    不先把台账记进 4101，④-c 的贷方 4101 就没有对应借方 —— 4101 会变成负数余额。
//!    有台账行才生成（`Option<Uuid>`），没有就完全不出现。
//!
//! # 幂等
//!
//! 批次级闸：该 `cost_period_id` 已有**未红冲**的 `source_type='COST_PERIOD'` 凭证 ⇒
//! **409 + `66074`**（与 B3-4 的完工入库闸同码同动作：这张批次已经结算过，要重算请先红冲）。
//! 红冲后（`reversed_by IS NOT NULL` / `is_red=1`）不算，可重新结算 —— 与 §4.8「红冲后回到未结算」一致。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher, STATUS_POSTED};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与 §4.7 / V0375 科目表逐字对齐，单测钉死） ====================

/// 借：生产成本-直接材料（完工结转贷方要素之一）。
pub const ACCOUNT_MATERIAL: &str = "4001.01";
/// 借：生产成本-直接人工（人工计提借方 / 完工结转贷方要素之一）。
pub const ACCOUNT_LABOR: &str = "4001.02";
/// 借：生产成本-制造费用（制造费用结转借方 / 完工结转贷方要素之一）。
pub const ACCOUNT_OVERHEAD: &str = "4001.03";
/// 借：库存商品（完工入库结转借方）。
pub const ACCOUNT_FINISHED_GOODS: &str = "1405";
/// 贷：应付职工薪酬-工资（人工计提贷方，§4.7.2）。
pub const ACCOUNT_PAYROLL: &str = "2211.01";

/// 凭证来源类型（`biz_voucher.source_type`，无库层白名单，见 `voucher` 模块 264 行）。
pub const SOURCE_TYPE_COST_PERIOD: &str = "COST_PERIOD";

/// 自动凭证（落库即记账）。
pub const VOUCHER_TYPE_AUTO: &str = "AUTO";

/// 人工来源 = 工资单：人工计提凭证**不生成**（§4.7.2 末段 / D-13）。
pub const LABOR_SOURCE_PAYROLL: &str = "PAYROLL";

/// 金额标度（`NUMERIC(18,4)`）。
pub const MONEY_SCALE: u32 = 4;

/// 人工计提摘要前缀。
pub const SUMMARY_LABOR: &str = "成本结算 人工计提";
/// 制造费用结转摘要前缀。
pub const SUMMARY_OVERHEAD: &str = "成本结算 制造费用结转";
/// 手工费用台账摘要前缀。
pub const SUMMARY_LEDGER: &str = "成本结算 手工费用台账";
/// 完工入库结转摘要前缀。
pub const SUMMARY_FINISH: &str = "成本结算 完工入库结转";

// ==================== 结构 ====================

/// 计算单一行（凭证侧的只读投影：金额取自库里已结算的列，不重算）。
#[derive(Debug, Clone)]
pub struct ItemCostRow {
    /// `biz_cost_period_item.id`。
    pub item_id: Uuid,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（只用于摘要 / 报文，人不看 id）。
    pub object_no: String,
    /// 产出品（`1405` 借方的 `aux_material_id`）。
    pub product_id: Uuid,
    /// 完工入库仓（`1405` 借方的 `aux_warehouse_id`）。
    pub warehouse_id: Uuid,
    /// 本期完工数量。
    pub completed_qty: Decimal,
    /// 本期材料投入。
    pub mat_cur: Decimal,
    /// 本期人工投入。
    pub labor_cur: Decimal,
    /// 本期制造费用投入（含直接行 + 分摊行）。
    pub oh_cur: Decimal,
    /// 单位材料成本（6 位）。
    pub unit_material: Decimal,
    /// 单位人工成本（6 位）。
    pub unit_labor: Decimal,
    /// 单位制造费用（6 位）。
    pub unit_overhead: Decimal,
    /// 完工成本（权威金额，= 入库金额）。
    pub finished_cost: Decimal,
    /// 期末在制成本（B3-6 / 巡检 C-COST-3 用）。
    pub wip_end_cost: Decimal,
}

/// 一个车间的工资金额（人工计提的一行借方）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaborGroup {
    /// 车间（`aux_dept_id`；空 = 工时行没带车间，仍要进账，不丢钱）。
    pub dept_id: Option<Uuid>,
    /// 车间名（`sys_dept` 实时名优先，回落归集行快照）。
    pub dept_name: Option<String>,
    /// 该车间当期人工金额。
    pub amount: Decimal,
}

/// 一个 (车间, 科目) 的制造费用金额（制造费用结转的一行借方）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverheadGroup {
    /// 车间。
    pub dept_id: Option<Uuid>,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 被摊 / 被结转的制造费用科目（`4101.*`；贷方按它清零）。
    pub account_code: String,
    /// 金额。
    pub amount: Decimal,
}

/// 一条手工费用台账的凭证行来源（借 4101.xx / 贷 对方科目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerGroup {
    /// 车间。
    pub dept_id: Option<Uuid>,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 借方（`biz_cost_overhead.debit_account_code`，`4101.*`）。
    pub debit_account: String,
    /// 贷方（`biz_cost_overhead.credit_account_code`，对方科目）。
    pub credit_account: String,
    /// 金额。
    pub amount: Decimal,
}

/// 完工结转的逐要素拆分（贷方三行的来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedSplit {
    /// 完工材料（贷 4001.01）。
    pub material: Decimal,
    /// 完工人工（贷 4001.02）。
    pub labor: Decimal,
    /// 完工制造费用（贷 4001.03，含尾差）。
    pub overhead: Decimal,
}

impl FinishedSplit {
    /// 三要素合计（必须 == `finished_cost`）。
    pub fn total(&self) -> Decimal {
        self.material + self.labor + self.overhead
    }
}

/// 结算上下文（期间 + 会计日期 + 操作人）。
#[derive(Debug, Clone)]
pub struct VoucherContext<'a> {
    /// 结算批次 id（凭证的 `source_id`）。
    pub period_id: Uuid,
    /// 期间码 `yyyy-MM`（归集行 / 计算单的期间键，也是凭证摘要的一部分）。
    pub period_code: &'a str,
    /// 批次号 `CS-yyyyMM-NNNN`（凭证 `source_no`，红冲按它反查）。
    pub batch_no: &'a str,
    /// 凭证日期（必须落在 `period_code` 内，`post_voucher` 会按它反查期间）。
    pub voucher_date: NaiveDate,
    /// 人工来源（`MANUAL` / `PAYROLL`）。
    pub labor_source: &'a str,
    /// 操作人（`posted_by` / 留痕）。
    pub operator: Option<Uuid>,
    /// 业务时间。
    pub now: DateTime<Utc>,
}

/// 一次结算发出的凭证（B3-6 要拿它回写 `biz_cost_period` 的三个 voucher_id + 事务内断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledVouchers {
    /// 人工计提（`labor_source='PAYROLL'` 或金额 0 ⇒ `None`）。
    pub labor: Option<Uuid>,
    /// 人工计提金额（= Σ`item.labor_cur`）。
    pub labor_total: Decimal,
    /// 制造费用结转（金额 0 ⇒ `None`）。
    pub overhead: Option<Uuid>,
    /// 制造费用结转金额（= Σ`item.oh_cur`）。
    pub overhead_total: Decimal,
    /// 手工费用台账凭证（没有台账行 ⇒ `None`）。
    pub ledger: Option<Uuid>,
    /// 完工入库结转（结算必备，`ck_biz_cost_period_settle` 要求非空）。
    pub finish: Uuid,
    /// 完工金额（= Σ`item.finished_cost` = Σ入库单金额）。
    pub finish_total: Decimal,
    /// 完工三要素（材料, 人工, 费用）—— 贷方三行的金额，逐项合计 == `finish_total`。
    pub finished_by_element: (Decimal, Decimal, Decimal),
}

// ==================== 纯逻辑 ====================

/// 4 位金额（`NUMERIC(18,4)`）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 把「完工数量 × 单位要素成本」拆成贷方三要素，差额按 费用 → 材料 → 人工 吸收。
///
/// 差额来源只有「期末在制数量 0 ⇒ 尾差进完工」（B2-11 §4.6.2 ②），量级 ≤ 0.001；
/// 吸收顺序的理由见模块头。三条吸收路径都放不下（差额大到某个要素要变负）时返回 409 +
/// `66076`：那是计算单与单位成本**互相矛盾**（不是舍入问题），不能硬造负数的贷方行。
pub fn split_finished(item: &ItemCostRow) -> std::result::Result<FinishedSplit, ApiError> {
    let qty = item.completed_qty;
    let mut material = money(qty * item.unit_material);
    let mut labor = money(qty * item.unit_labor);
    let mut overhead = money(qty * item.unit_overhead);
    let residual = item.finished_cost - (material + labor + overhead);
    if !residual.is_zero() {
        if overhead + residual >= Decimal::ZERO {
            overhead += residual;
        } else if material + residual >= Decimal::ZERO {
            material += residual;
        } else if labor + residual >= Decimal::ZERO {
            labor += residual;
        } else {
            return Err(mismatch_error(&format!(
                "成本对象 {} 的完工成本 {} 比「完工数量 {} × 单位要素成本」合计 {} 小 {}，\
                 差额大到贷方要素要变负 —— 计算单与单位成本互相矛盾，请重算该期间",
                item.object_no,
                item.finished_cost,
                qty,
                material + labor + overhead,
                (material + labor + overhead) - item.finished_cost
            )));
        }
    }
    let split = FinishedSplit {
        material,
        labor,
        overhead,
    };
    debug_assert_eq!(split.total(), money(item.finished_cost));
    Ok(split)
}

/// 贷方三行（跳过 0 元行；`post_voucher` 不接受 0 元分录的风格由调用方保证）。
pub fn credit_specs(splits: &[FinishedSplit]) -> Vec<(String, Decimal)> {
    let mut material = Decimal::ZERO;
    let mut labor = Decimal::ZERO;
    let mut overhead = Decimal::ZERO;
    for split in splits {
        material += split.material;
        labor += split.labor;
        overhead += split.overhead;
    }
    [
        (ACCOUNT_MATERIAL.to_string(), material),
        (ACCOUNT_LABOR.to_string(), labor),
        (ACCOUNT_OVERHEAD.to_string(), overhead),
    ]
    .into_iter()
    .filter(|(_, amount)| !amount.is_zero())
    .collect()
}

/// 按科目把 (车间, 科目) 的金额合并（制造费用结转的贷方：一科目一行，跨车间合计）。
pub fn merge_by_account(groups: &[OverheadGroup]) -> Vec<(String, Decimal)> {
    let mut map: std::collections::BTreeMap<String, Decimal> = std::collections::BTreeMap::new();
    for group in groups {
        *map.entry(group.account_code.clone()).or_insert(Decimal::ZERO) += group.amount;
    }
    map.into_iter().collect()
}

/// 车间名：`sys_dept` 实时名优先，回落归集行快照（AGENTS 约定 13：业务展示 join 实时名）。
fn dept_label(dept_id: Option<Uuid>, live: Option<String>, snapshot: Option<String>) -> String {
    match (live, snapshot) {
        (Some(name), _) if !name.is_empty() => name,
        (_, Some(name)) if !name.is_empty() => name,
        _ => dept_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "未指定车间".to_string()),
    }
}

// ==================== 错误构造 ====================

fn mismatch_error(message: &str) -> ApiError {
    ApiError::new(409, error_code::ERP_COST_VOUCHER_MISMATCH, message.to_string())
}

fn account_of(account: Option<AccountRef>, code: &str) -> std::result::Result<AccountRef, ApiError> {
    account.ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "会计科目 {code} 不存在或已停用，无法生成成本结算凭证\
                 （请先在「会计科目」里补齐 §4.7 指定的末级科目）"
            ),
        )
    })
}

fn db_error(error: DbError, context: &str) -> ApiError {
    utils::db_error(error, context)
}

// ==================== 读：计算单 / 归集行 / 台账 ====================

/// 本批次的逐对象成本行（顺序稳定：对象号 + id）。
pub async fn batch_items(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<ItemCostRow>> {
    let rows = ex
        .query_sql(
            "SELECT i.id                    AS item_id, \
                    i.cost_object_id, \
                    i.product_id, \
                    i.completed_qty, \
                    i.mat_cur, \
                    i.labor_cur, \
                    i.oh_cur, \
                    i.unit_material, \
                    i.unit_labor, \
                    i.unit_overhead, \
                    i.finished_cost, \
                    i.wip_end_cost, \
                    o.object_no, \
                    o.target_warehouse_id \
               FROM biz_cost_period_item i \
               JOIN biz_cost_object o ON o.id = i.cost_object_id AND o.deleted_at IS NULL \
              WHERE i.period_id = ?::uuid \
              ORDER BY o.object_no ASC, i.id ASC"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ItemCostRow {
                item_id: row.get_typed("item_id")?,
                cost_object_id: row.get_typed("cost_object_id")?,
                object_no: row.get_typed("object_no")?,
                product_id: row.get_typed("product_id")?,
                warehouse_id: row.get_typed("target_warehouse_id")?,
                completed_qty: row.get_typed("completed_qty")?,
                mat_cur: row.get_typed("mat_cur")?,
                labor_cur: row.get_typed("labor_cur")?,
                oh_cur: row.get_typed("oh_cur")?,
                unit_material: row.get_typed("unit_material")?,
                unit_labor: row.get_typed("unit_labor")?,
                unit_overhead: row.get_typed("unit_overhead")?,
                finished_cost: row.get_typed("finished_cost")?,
                wip_end_cost: row.get_typed("wip_end_cost")?,
            })
        })
        .collect()
}

/// 当期人工（按车间汇总，**只算本批次对象**）。
///
/// 只取本批次对象是为了与计算单的 `labor_cur` 同口径（`item.labor_cur = Σ collect LABOR`）：
/// 期外对象的人工留在自己批次里结算，不会把这张凭证撑大。
pub async fn labor_groups(ex: &impl SqlExecutor, period_id: Uuid, period_code: &str) -> DbResult<Vec<LaborGroup>> {
    let rows = ex
        .query_sql(
            "SELECT c.dept_id, \
                    max(d.dept_name)  AS live_dept_name, \
                    max(c.dept_name)  AS snapshot_dept_name, \
                    sum(c.amount)     AS amount \
               FROM biz_cost_collect c \
               LEFT JOIN sys_dept d ON d.id = c.dept_id \
              WHERE c.period_code = ? \
                AND c.element = 'LABOR' \
                AND c.deleted_at IS NULL \
                AND c.cost_object_id IN \
                    (SELECT cost_object_id FROM biz_cost_period_item WHERE period_id = ?::uuid) \
              GROUP BY c.dept_id \
              ORDER BY c.dept_id NULLS LAST"
                .to_string(),
            vec![Value::Text(period_code.to_string()), Value::from(&period_id)],
        )
        .await?;
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let dept_id: Option<Uuid> = row.get_typed("dept_id")?;
        let live: Option<String> = row.get_typed("live_dept_name")?;
        let snapshot: Option<String> = row.get_typed("snapshot_dept_name")?;
        let name = dept_label(dept_id, live, snapshot);
        groups.push(LaborGroup {
            dept_id,
            dept_name: Some(name),
            amount: row.get_typed("amount")?,
        });
    }
    Ok(groups)
}

/// 当期制造费用（按 (车间, 科目) 汇总）：**池行 + 直接行**（偏差 1，见模块头）。
///
/// 科目从 `biz_cost_collect.source_type` 反查来源表（归集表没有 `account_code` 列）：
/// `OVERHEAD_TICKET → biz_cost_overhead.debit_account_code`、`DEPRECIATION → biz_depreciation.expense_account_code`、
/// `EXPENSE → biz_expense_item.account_code`。`ALLOC` 行**不算**（它是池的去向，算进来会自我循环）。
pub async fn overhead_groups(
    ex: &impl SqlExecutor,
    period_id: Uuid,
    period_code: &str,
) -> DbResult<Vec<OverheadGroup>> {
    let rows = ex
        .query_sql(
            "SELECT c.dept_id, \
                    max(d.dept_name) AS live_dept_name, \
                    max(c.dept_name) AS snapshot_dept_name, \
                    COALESCE(oh.debit_account_code, dp.expense_account_code, ei.account_code) AS account_code, \
                    sum(c.amount)    AS amount \
               FROM biz_cost_collect c \
               LEFT JOIN biz_cost_overhead oh \
                      ON c.source_type = 'OVERHEAD_TICKET' AND oh.id = c.source_id \
               LEFT JOIN biz_depreciation dp \
                      ON c.source_type = 'DEPRECIATION' AND dp.id = c.source_id \
               LEFT JOIN biz_expense_item ei \
                      ON c.source_type = 'EXPENSE' AND ei.id = c.source_id \
               LEFT JOIN sys_dept d ON d.id = c.dept_id \
              WHERE c.period_code = ? \
                AND c.element = 'OVERHEAD' \
                AND c.deleted_at IS NULL \
                AND c.source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET') \
                AND (c.cost_object_id IS NULL \
                     OR c.cost_object_id IN \
                        (SELECT cost_object_id FROM biz_cost_period_item WHERE period_id = ?::uuid)) \
              GROUP BY c.dept_id, \
                       COALESCE(oh.debit_account_code, dp.expense_account_code, ei.account_code) \
              ORDER BY c.dept_id NULLS LAST, account_code ASC"
                .to_string(),
            vec![Value::Text(period_code.to_string()), Value::from(&period_id)],
        )
        .await?;
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let dept_id: Option<Uuid> = row.get_typed("dept_id")?;
        let live: Option<String> = row.get_typed("live_dept_name")?;
        let snapshot: Option<String> = row.get_typed("snapshot_dept_name")?;
        let account_code: Option<String> = row.get_typed("account_code")?;
        let Some(account_code) = account_code else {
            return Err(DbError::InvalidValue(format!(
                "制造费用归集行缺少科目：车间 {} 有费归集行但来源单据（手工台账 / 折旧 / 报销）已不存在，\
                 无法把 4101 清零 —— 请先重算该期间的费归集（calc 第 3 段）",
                dept_label(dept_id, live.clone(), snapshot.clone())
            )));
        };
        groups.push(OverheadGroup {
            dept_id,
            dept_name: Some(dept_label(dept_id, live, snapshot)),
            account_code,
            amount: row.get_typed("amount")?,
        });
    }
    Ok(groups)
}

/// 当期手工费用台账（按 (车间, 借科目, 贷科目) 汇总）—— 「借 4101 / 贷 对方科目」的来源。
pub async fn ledger_groups(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<LedgerGroup>> {
    let rows = ex
        .query_sql(
            "SELECT t.dept_id, \
                    max(d.dept_name) AS live_dept_name, \
                    max(t.dept_name) AS snapshot_dept_name, \
                    t.debit_account_code, \
                    t.credit_account_code, \
                    sum(t.amount)    AS amount \
               FROM biz_cost_overhead t \
               LEFT JOIN sys_dept d ON d.id = t.dept_id \
              WHERE t.period_code = ? \
                AND t.deleted_at IS NULL \
              GROUP BY t.dept_id, t.debit_account_code, t.credit_account_code \
              ORDER BY t.debit_account_code ASC, t.credit_account_code ASC, t.dept_id ASC"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let dept_id: Option<Uuid> = row.get_typed("dept_id")?;
        let live: Option<String> = row.get_typed("live_dept_name")?;
        let snapshot: Option<String> = row.get_typed("snapshot_dept_name")?;
        groups.push(LedgerGroup {
            dept_id,
            dept_name: Some(dept_label(dept_id, live, snapshot)),
            debit_account: row.get_typed("debit_account_code")?,
            credit_account: row.get_typed("credit_account_code")?,
            amount: row.get_typed("amount")?,
        });
    }
    Ok(groups)
}

/// 该批次已发出的（未红冲的蓝字）凭证 —— B3-7 红冲要逐个 `reverse_posted`。
pub async fn voucher_ids_of(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND deleted_at IS NULL AND is_red = 0 \
              ORDER BY voucher_no ASC, id ASC"
                .to_string(),
            vec![
                Value::Text(SOURCE_TYPE_COST_PERIOD.to_string()),
                Value::from(&period_id),
            ],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("id")).collect()
}

/// 幂等闸：该批次是否已经发过（未红冲的）结算凭证。
async fn ensure_not_settled(ex: &impl SqlExecutor, period_id: Uuid) -> std::result::Result<(), ApiError> {
    let existing: i64 = ex
        .query_first_sql(
            "SELECT count(*)::bigint AS n FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND deleted_at IS NULL AND is_red = 0 AND reversed_by IS NULL"
                .to_string(),
            vec![
                Value::Text(SOURCE_TYPE_COST_PERIOD.to_string()),
                Value::from(&period_id),
            ],
        )
        .await
        .map_err(|error| db_error(error, "结算凭证：查该批次是否已发凭证"))?
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()?
        .unwrap_or(0);
    if existing > 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_FINISH_ALREADY_SETTLED,
            format!("结算批次已经生成过 {existing} 张结算凭证，不会重复记账（要重算请先红冲该批次）"),
        ));
    }
    Ok(())
}

// ==================== 写：四张凭证 ====================

/// **结算 ④**：一次发齐本批次的三张（必要时四张）凭证 + 回写入库单的 `voucher_id`。
///
/// 调用方（B3-6）必须：
/// * 先把批次行 `SELECT … FOR UPDATE`、校验 `status='APPROVED'`；
/// * 在**同一个事务**里先跑完工入库（B3-4）、再调用本函数；
/// * 不要自己再 commit —— 本文件只落语句。
pub async fn post_all(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
) -> std::result::Result<SettledVouchers, ApiError> {
    ensure_voucher_date_in_period(ctx)?;
    ensure_not_settled(ex, ctx.period_id).await?;

    let items = batch_items(ex, ctx.period_id)
        .await
        .map_err(|error| db_error(error, "结算凭证：读计算单"))?;

    let labor = post_labor_voucher(ex, ctx, &items).await?;
    let ledger = post_ledger_voucher(ex, ctx).await?;
    let overhead = post_overhead_voucher(ex, ctx, &items).await?;
    let (finish, finish_total, finished_by_element) = post_finish_voucher(ex, ctx, &items).await?;

    Ok(SettledVouchers {
        labor: labor.map(|(id, _)| id),
        labor_total: labor.map(|(_, total)| total).unwrap_or(Decimal::ZERO),
        overhead: overhead.map(|(id, _)| id),
        overhead_total: overhead.map(|(_, total)| total).unwrap_or(Decimal::ZERO),
        ledger,
        finish,
        finish_total,
        finished_by_element,
    })
}

/// ① 人工计提（§4.7.2）：借 4001.02（按车间，aux_dept_id）贷 2211.01，同额。
///
/// * `labor_source='PAYROLL'` ⇒ **不生成**（工资凭证由薪资单出，D-13）；
/// * 当期人工合计为 0 ⇒ 不生成（`post_voucher` 拒 0 元凭证）；
/// * Σ借 ≠ Σ`item.labor_cur` ⇒ 409 + `66076`（计算单与归集行不一致，必须先重算）。
pub async fn post_labor_voucher(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
    items: &[ItemCostRow],
) -> std::result::Result<Option<(Uuid, Decimal)>, ApiError> {
    if ctx.labor_source == LABOR_SOURCE_PAYROLL {
        return Ok(None);
    }
    let groups = labor_groups(ex, ctx.period_id, ctx.period_code)
        .await
        .map_err(|error| db_error(error, "结算凭证：读当期人工归集"))?;
    let total = money(groups.iter().map(|group| group.amount).sum());
    if total.is_zero() {
        return Ok(None);
    }
    let item_total = money(items.iter().map(|item| item.labor_cur).sum());
    if total != item_total {
        return Err(mismatch_error(&format!(
            "人工计提凭证金额 {total} 与计算单人工合计 {item_total} 不一致\
             （归集行被改过或计算单不是最后一次 calc 的结果），请先重算该期间的成本计算单"
        )));
    }
    let debit_account = account_of(
        voucher::account_by_code(ex, ACCOUNT_LABOR)
            .await
            .map_err(|error| db_error(error, "结算凭证：取科目 4001.02"))?,
        ACCOUNT_LABOR,
    )?;
    let credit_account = account_of(
        voucher::account_by_code(ex, ACCOUNT_PAYROLL)
            .await
            .map_err(|error| db_error(error, "结算凭证：取科目 2211.01"))?,
        ACCOUNT_PAYROLL,
    )?;

    let summary = format!("{SUMMARY_LABOR} · 批次 {} · 期间 {}", ctx.batch_no, ctx.period_code);
    let mut specs = Vec::with_capacity(groups.len() + 1);
    for group in &groups {
        specs.push(LineSpec {
            summary: format!(
                "{SUMMARY_LABOR} · 车间 {} · 期间 {}",
                group.dept_name.as_deref().unwrap_or("未指定车间"),
                ctx.period_code
            ),
            account: debit_account.clone(),
            debit: group.amount,
            credit: Decimal::ZERO,
            dept: group.dept_id,
            material: None,
            warehouse: None,
        });
    }
    specs.push(LineSpec {
        summary: format!("{SUMMARY_LABOR} · 计提当期工资 · 期间 {}", ctx.period_code),
        account: credit_account,
        debit: Decimal::ZERO,
        credit: total,
        dept: None,
        material: None,
        warehouse: None,
    });
    let id = post(
        ex,
        ctx,
        &summary,
        "结转当期直接人工（按车间汇总，l_car 来自工时台账）",
        specs,
    )
    .await?;
    Ok(Some((id, total)))
}

/// ② 手工费用台账「借 4101 / 贷 对方科目」（§4.7.3 第二段 / §4.8 ④「按需」）。
///
/// 没有台账行 ⇒ `None`（折旧 / 报销在上游已借 4101，不需要这一步）。
pub async fn post_ledger_voucher(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let groups = ledger_groups(ex, ctx.period_code)
        .await
        .map_err(|error| db_error(error, "结算凭证：读手工费用台账"))?;
    if groups.is_empty() {
        return Ok(None);
    }
    let mut specs = Vec::with_capacity(groups.len() * 2);
    for group in &groups {
        let debit_account = account_of(
            voucher::account_by_code(ex, &group.debit_account)
                .await
                .map_err(|error| db_error(error, "结算凭证：取台账借科目"))?,
            &group.debit_account,
        )?;
        let credit_account = account_of(
            voucher::account_by_code(ex, &group.credit_account)
                .await
                .map_err(|error| db_error(error, "结算凭证：取台账贷科目"))?,
            &group.credit_account,
        )?;
        let dept = group.dept_name.as_deref().unwrap_or("未指定车间");
        specs.push(LineSpec {
            summary: format!("{SUMMARY_LEDGER} · 车间 {dept} · 期间 {}", ctx.period_code),
            account: debit_account,
            debit: group.amount,
            credit: Decimal::ZERO,
            dept: group.dept_id,
            material: None,
            warehouse: None,
        });
        specs.push(LineSpec {
            summary: format!("{SUMMARY_LEDGER} · 车间 {dept} · 期间 {}", ctx.period_code),
            account: credit_account,
            debit: Decimal::ZERO,
            credit: group.amount,
            dept: group.dept_id,
            material: None,
            warehouse: None,
        });
    }
    let summary = format!("{SUMMARY_LEDGER} · 批次 {} · 期间 {}", ctx.batch_no, ctx.period_code);
    let id = post(
        ex,
        ctx,
        &summary,
        "手工费用台账入账：借制造费用（4101.*）/ 贷对方科目，随后由制造费用结转凭证清零",
        specs,
    )
    .await?;
    Ok(Some(id))
}

/// ③ 制造费用结转（§4.7.3，偏差 1 见模块头）：借 4001.03（按 (车间, 科目)，aux_dept_id）
/// 贷 4101.xx（按科目清零），同额。
///
/// * 合计 0 ⇒ 不生成；
/// * Σ借 ≠ Σ`item.oh_cur` ⇒ 409 + `66076`（池 / 直接行与计算单不一致 —— 通常是池没摊完）。
pub async fn post_overhead_voucher(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
    items: &[ItemCostRow],
) -> std::result::Result<Option<(Uuid, Decimal)>, ApiError> {
    let groups = overhead_groups(ex, ctx.period_id, ctx.period_code)
        .await
        .map_err(|error| db_error(error, "结算凭证：读当期制造费用归集"))?;
    let total = money(groups.iter().map(|group| group.amount).sum());
    if total.is_zero() {
        return Ok(None);
    }
    let item_total = money(items.iter().map(|item| item.oh_cur).sum());
    if total != item_total {
        return Err(mismatch_error(&format!(
            "制造费用结转凭证金额 {total} 与计算单制造费用合计 {item_total} 不一致\
             （池没摊完 / 有费归集行没进对象 / 计算单不是最后一次 calc 的结果），\
             请先补规则重算该期间"
        )));
    }
    let debit_account = account_of(
        voucher::account_by_code(ex, ACCOUNT_OVERHEAD)
            .await
            .map_err(|error| db_error(error, "结算凭证：取科目 4001.03"))?,
        ACCOUNT_OVERHEAD,
    )?;
    let mut specs = Vec::with_capacity(groups.len() * 2);
    for group in &groups {
        let account = account_of(
            voucher::account_by_code(ex, &group.account_code)
                .await
                .map_err(|error| db_error(error, "结算凭证：取制造费用科目"))?,
            &group.account_code,
        )?;
        let dept = group.dept_name.as_deref().unwrap_or("未指定车间");
        specs.push(LineSpec {
            summary: format!(
                "{SUMMARY_OVERHEAD} · 车间 {dept} · 科目 {} · 期间 {}",
                group.account_code, ctx.period_code
            ),
            account: debit_account.clone(),
            debit: group.amount,
            credit: Decimal::ZERO,
            dept: group.dept_id,
            material: None,
            warehouse: None,
        });
        specs.push(LineSpec {
            summary: format!("{SUMMARY_OVERHEAD} · 清零 {}（车间 {dept}）", group.account_code),
            account,
            debit: Decimal::ZERO,
            credit: group.amount,
            dept: group.dept_id,
            material: None,
            warehouse: None,
        });
    }
    let summary = format!("{SUMMARY_OVERHEAD} · 批次 {} · 期间 {}", ctx.batch_no, ctx.period_code);
    let id = post(
        ex,
        ctx,
        &summary,
        "制造费用全部进产品成本：借 4001.03 / 贷 4101.*（含直接行，4101 期末余额为 0）",
        specs,
    )
    .await?;
    Ok(Some((id, total)))
}

/// ④ 完工入库结转（§4.7.4）：借 1405（按成本对象，aux 物料+仓库）贷 4001.01/.02/.03（按要素）。
///
/// 生成后**回写入库单**：该批次非作废的 `biz_stock_in.voucher_id` 指向这张凭证（§4.7.4 末段，
/// 多张入库单共指一张结算凭证 —— 该列无唯一约束）。
///
/// 完工金额合计 0 ⇒ **409 + `66075`**：`ck_biz_cost_period_settle` 要求 `SETTLED` 必须有
/// `finish_voucher_id`，没有完工结转的批次不可能合法结算（先录产量、或本期间不结算）。
pub async fn post_finish_voucher(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
    items: &[ItemCostRow],
) -> std::result::Result<(Uuid, Decimal, (Decimal, Decimal, Decimal)), ApiError> {
    let mut splits = Vec::with_capacity(items.len());
    for item in items {
        splits.push(split_finished(item)?);
    }
    let finish_total = money(items.iter().map(|item| item.finished_cost).sum());
    if finish_total.is_zero() {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_FINISH_VOUCHER_EMPTY,
            format!(
                "批次 {} 期间 {} 的完工成本合计为 0，没有可结转的完工入库金额 —— \
                 结算批次必须有完工结转凭证（库层 ck_biz_cost_period_settle）。\
                 请先录入本期完工数量并重算，或本期间暂不结算",
                ctx.batch_no, ctx.period_code
            ),
        ));
    }
    let debit_account = account_of(
        voucher::account_by_code(ex, ACCOUNT_FINISHED_GOODS)
            .await
            .map_err(|error| db_error(error, "结算凭证：取科目 1405"))?,
        ACCOUNT_FINISHED_GOODS,
    )?;
    let credits = credit_specs(&splits);
    let mut specs = Vec::with_capacity(items.len() + credits.len());
    let mut credit_sum = Decimal::ZERO;
    for (code, amount) in &credits {
        credit_sum += *amount;
        let account = account_of(
            voucher::account_by_code(ex, code)
                .await
                .map_err(|error| db_error(error, "结算凭证：取生产成本末级科目"))?,
            code,
        )?;
        specs.push(LineSpec {
            summary: format!("{SUMMARY_FINISH} · 产出品完工转出 · 期间 {}", ctx.period_code),
            account,
            debit: Decimal::ZERO,
            credit: *amount,
            dept: None,
            material: None,
            warehouse: None,
        });
    }
    if credit_sum != finish_total {
        // split_finished 已保证逐行相等，这里是「所有行加起来」的兜底（4 位加法不会漂）
        return Err(mismatch_error(&format!(
            "完工结转贷方要素合计 {credit_sum} ≠ 完工成本合计 {finish_total}（请重算该期间）"
        )));
    }
    for (item, split) in items.iter().zip(splits.iter()) {
        if item.finished_cost.is_zero() {
            continue;
        }
        specs.push(LineSpec {
            summary: format!(
                "{SUMMARY_FINISH} · 成本对象 {} · 数量 {} · 期间 {}",
                item.object_no, item.completed_qty, ctx.period_code
            ),
            account: debit_account.clone(),
            debit: item.finished_cost,
            credit: Decimal::ZERO,
            dept: None,
            material: Some(item.product_id),
            warehouse: Some(item.warehouse_id),
        });
        debug_assert_eq!(split.total(), money(item.finished_cost));
    }
    let summary = format!("{SUMMARY_FINISH} · 批次 {} · 期间 {}", ctx.batch_no, ctx.period_code);
    let id = post(
        ex,
        ctx,
        &summary,
        "完工入库结转：借 1405（按成本对象）/ 贷 4001.01·.02·.03（按要素），同时是完工入库单的凭证",
        specs,
    )
    .await?;

    // 回写入库单凭证指针（§4.7.4：多张入库单共指一张结算凭证；作废单不挂）
    ex.exec_sql(
        "UPDATE biz_stock_in SET voucher_id = ?::uuid, updated_at = ? \
          WHERE cost_period_id = ?::uuid AND deleted_at IS NULL AND status <> 'CANCELLED'"
            .to_string(),
        vec![Value::from(&id), Value::from(ctx.now), Value::from(&ctx.period_id)],
    )
    .await
    .map_err(|error| db_error(error, "结算凭证：回写入库单凭证号"))?;

    let (material, labor, overhead) = credits.iter().fold(
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        |(material, labor, overhead), (code, amount)| match code.as_str() {
            ACCOUNT_MATERIAL => (material + *amount, labor, overhead),
            ACCOUNT_LABOR => (material, labor + *amount, overhead),
            ACCOUNT_OVERHEAD => (material, labor, overhead + *amount),
            _ => (material, labor, overhead),
        },
    );
    Ok((id, finish_total, (material, labor, overhead)))
}

// ==================== 内部：分录构造 ====================

/// 一条待落分录（line_no 由 [`build_lines`] 统一编号，避免手写错位）。
struct LineSpec {
    summary: String,
    account: AccountRef,
    debit: Decimal,
    credit: Decimal,
    dept: Option<Uuid>,
    material: Option<Uuid>,
    warehouse: Option<Uuid>,
}

fn build_lines(specs: Vec<LineSpec>) -> Vec<EntryLine> {
    specs
        .into_iter()
        .enumerate()
        .map(|(index, spec)| EntryLine {
            line_no: i32::try_from(index + 1).unwrap_or(1),
            summary: spec.summary,
            account: spec.account,
            debit: spec.debit,
            credit: spec.credit,
            aux_supplier_id: None,
            aux_material_id: spec.material,
            aux_warehouse_id: spec.warehouse,
            aux_dept_id: spec.dept,
        })
        .collect()
}

/// 一张凭证的落库（唯一收口：`voucher::post_voucher`；取号 / 平衡 / 末级科目 / 期间闸都在那里）。
async fn post(
    ex: &impl SqlExecutor,
    ctx: &VoucherContext<'_>,
    summary: &str,
    remark: &str,
    specs: Vec<LineSpec>,
) -> std::result::Result<Uuid, ApiError> {
    if specs.len() < 2 {
        return Err(mismatch_error(&format!(
            "「{summary}」只有 {} 条分录，不构成一张凭证（0 元凭证应当在生成前就被跳过）",
            specs.len()
        )));
    }
    let lines = build_lines(specs);
    voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: SOURCE_TYPE_COST_PERIOD,
            source_id: Some(ctx.period_id),
            // `source_no` 是来源单号（varchar(32)）：结算批次的来源就是批次号本身
            source_no: Some(ctx.batch_no),
            source_doc_no: Some(ctx.batch_no),
            voucher_date: ctx.voucher_date,
            summary: summary.to_string(),
            lines,
            remark: Some(remark),
            red_of: None,
            status: STATUS_POSTED,
        },
        ctx.operator,
        ctx.now,
    )
    .await
    .map_err(|error| db_error(error, &format!("结算凭证：落「{summary}」")))
}

/// 凭证日期必须落在结算期间内（`post_voucher` 会按日期反查会计期间，
/// 日期与期间码错月会让凭证落到隔壁月份 —— 账表对不上，提前给人话）。
fn ensure_voucher_date_in_period(ctx: &VoucherContext<'_>) -> std::result::Result<(), ApiError> {
    let month = ctx.voucher_date.format("%Y-%m").to_string();
    if month == ctx.period_code {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!(
            "结算凭证日期 {}（{}）与结算期间 {} 不在同一个月：凭证会落到隔壁会计期间，\
             请把凭证日期改到期间内",
            ctx.voucher_date, month, ctx.period_code
        ),
    ))
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn fmt(value: Decimal, scale: usize) -> String {
        format!("{:.*}", scale, value)
    }

    /// 设计 §4.6.2 算例 A 的计算单行（完工 800 / 在制 200）。
    fn example_a() -> ItemCostRow {
        ItemCostRow {
            item_id: Uuid::now_v7(),
            cost_object_id: Uuid::now_v7(),
            object_no: "CB-A-001".to_string(),
            product_id: Uuid::now_v7(),
            warehouse_id: Uuid::now_v7(),
            completed_qty: dec("800"),
            mat_cur: dec("19000"),
            labor_cur: dec("4000"),
            oh_cur: dec("4000"),
            unit_material: dec("19.000000"),
            unit_labor: dec("4.444444"),
            unit_overhead: dec("4.444444"),
            finished_cost: dec("22311.1104"),
            wip_end_cost: dec("4688.8896"),
        }
    }

    /// §4.7.5 表的贷方三行：15200 / 3555.5552 / 3555.5552，合计 22311.1104。
    #[test]
    fn example_a_splits_into_the_design_numbers() {
        let split = split_finished(&example_a()).expect("拆分");
        assert_eq!(fmt(split.material, 4), "15200.0000");
        assert_eq!(fmt(split.labor, 4), "3555.5552");
        assert_eq!(fmt(split.overhead, 4), "3555.5552");
        assert_eq!(fmt(split.total(), 4), "22311.1104");
    }

    /// 期末在制 0 件 ⇒ 尾差进完工（B2-11 跨月用例）：差额落制造费用，三行合计仍分毫不差。
    #[test]
    fn tail_difference_is_absorbed_by_overhead() {
        let item = ItemCostRow {
            completed_qty: dec("500"),
            unit_material: dec("17.600000"),
            unit_labor: dec("4.444444"),
            unit_overhead: dec("4.444444"),
            // 500 × (17.6 + 4.444444 + 4.444444) = 13244.4440；给完工多 0.0008（设计里的尾差形态）
            finished_cost: dec("13244.4448"),
            ..example_a()
        };
        let split = split_finished(&item).expect("拆分");
        assert_eq!(fmt(split.material, 4), "8800.0000");
        assert_eq!(fmt(split.labor, 4), "2222.2220", "500 × 4.444444 截到 4 位");
        assert_eq!(fmt(split.overhead, 4), "2222.2228", "尾差 0.0008 落制造费用");
        assert_eq!(split.total(), dec("13244.4448"));
    }

    /// 尾差为负（完工比公式小）同样能吸收，且不会出现负数贷方行。
    #[test]
    fn negative_tail_is_absorbed_without_negative_lines() {
        let item = ItemCostRow {
            completed_qty: dec("100"),
            unit_material: dec("10.000000"),
            unit_labor: dec("2.000000"),
            unit_overhead: dec("0"),
            finished_cost: dec("1199.9999"),
            ..example_a()
        };
        let split = split_finished(&item).expect("拆分");
        assert_eq!(fmt(split.material, 4), "999.9999", "费用为 0 ⇒ 差额退到材料");
        assert_eq!(fmt(split.labor, 4), "200.0000");
        assert_eq!(fmt(split.overhead, 4), "0.0000");
        assert_eq!(split.total(), dec("1199.9999"));
    }

    /// 完工成本远小于公式（三个要素都放不下差额）⇒ 409 + 66076，不造负数贷方行。
    #[test]
    fn impossible_split_is_rejected_with_66076() {
        let item = ItemCostRow {
            completed_qty: dec("100"),
            unit_material: dec("10.000000"),
            unit_labor: dec("2.000000"),
            unit_overhead: dec("1.000000"),
            // 公式合计 1300，完工只给 100：任何一个要素都吸收不了 −1200
            finished_cost: dec("100"),
            ..example_a()
        };
        let error = split_finished(&item).expect_err("应拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_VOUCHER_MISMATCH);
    }

    /// 贷方三行：0 元要素不出行；跨对象按要素相加。
    #[test]
    fn credit_specs_merge_objects_and_skip_zero() {
        let splits = vec![
            FinishedSplit {
                material: dec("15200.0000"),
                labor: dec("3555.5552"),
                overhead: dec("3555.5552"),
            },
            FinishedSplit {
                material: dec("100.0000"),
                labor: Decimal::ZERO,
                overhead: dec("0.0004"),
            },
        ];
        let credits = credit_specs(&splits);
        assert_eq!(
            credits,
            vec![
                (ACCOUNT_MATERIAL.to_string(), dec("15300.0000")),
                (ACCOUNT_LABOR.to_string(), dec("3555.5552")),
                (ACCOUNT_OVERHEAD.to_string(), dec("3555.5556")),
            ]
        );
        let only_material = credit_specs(&[FinishedSplit {
            material: dec("10"),
            labor: Decimal::ZERO,
            overhead: Decimal::ZERO,
        }]);
        assert_eq!(only_material.len(), 1, "0 元要素不出行");
    }

    /// 制造费用贷方：同科目跨车间合并，科目编码升序（结果稳定，不随部门顺序漂）。
    #[test]
    fn merge_by_account_folds_departments() {
        let groups = vec![
            OverheadGroup {
                dept_id: None,
                dept_name: None,
                account_code: "4101.02".to_string(),
                amount: dec("1000"),
            },
            OverheadGroup {
                dept_id: None,
                dept_name: None,
                account_code: "4101.01".to_string(),
                amount: dec("2000"),
            },
            OverheadGroup {
                dept_id: None,
                dept_name: None,
                account_code: "4101.01".to_string(),
                amount: dec("1000"),
            },
        ];
        assert_eq!(
            merge_by_account(&groups),
            vec![
                ("4101.01".to_string(), dec("3000")),
                ("4101.02".to_string(), dec("1000")),
            ]
        );
    }

    /// 科目常量与 §4.7 / V0375 科目表逐字一致（改错一个字符账就记到别的科目上）。
    #[test]
    fn account_codes_match_the_design() {
        assert_eq!(ACCOUNT_MATERIAL, "4001.01");
        assert_eq!(ACCOUNT_LABOR, "4001.02");
        assert_eq!(ACCOUNT_OVERHEAD, "4001.03");
        assert_eq!(ACCOUNT_FINISHED_GOODS, "1405");
        assert_eq!(ACCOUNT_PAYROLL, "2211.01");
        assert_eq!(SOURCE_TYPE_COST_PERIOD, "COST_PERIOD");
        assert_eq!(LABOR_SOURCE_PAYROLL, "PAYROLL");
        assert_eq!(MONEY_SCALE, 4);
    }

    /// 凭证日期与期间码错月 ⇒ 400（不许把凭证落到隔壁月份）。
    #[test]
    fn voucher_date_outside_period_is_rejected() {
        let ctx = VoucherContext {
            period_id: Uuid::now_v7(),
            period_code: "2026-11",
            batch_no: "CS-202611-0001",
            voucher_date: NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期"),
            labor_source: "MANUAL",
            operator: None,
            now: Utc::now(),
        };
        let error = ensure_voucher_date_in_period(&ctx).expect_err("应拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        let ok = VoucherContext {
            voucher_date: NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期"),
            ..ctx
        };
        assert!(ensure_voucher_date_in_period(&ok).is_ok());
    }

    /// 分录行号连续从 1 起、借贷只落一侧（`post_voucher` 的平衡闸依赖它）。
    #[test]
    fn line_numbers_are_sequential() {
        let account = AccountRef {
            id: Uuid::now_v7(),
            code: ACCOUNT_LABOR.to_string(),
            name: "生产成本-直接人工".to_string(),
        };
        let lines = build_lines(vec![
            LineSpec {
                summary: "借".to_string(),
                account: account.clone(),
                debit: dec("4000"),
                credit: Decimal::ZERO,
                dept: None,
                material: None,
                warehouse: None,
            },
            LineSpec {
                summary: "贷".to_string(),
                account,
                debit: Decimal::ZERO,
                credit: dec("4000"),
                dept: None,
                material: None,
                warehouse: None,
            },
        ]);
        assert_eq!(lines.iter().map(|line| line.line_no).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(lines[0].debit, dec("4000"));
        assert_eq!(lines[1].credit, dec("4000"));
    }

    /// 车间名回落顺序：实时名 > 快照 > id。
    #[test]
    fn dept_label_prefers_live_name() {
        let id = Uuid::now_v7();
        assert_eq!(
            dept_label(Some(id), Some("一车间".to_string()), Some("旧车间".to_string())),
            "一车间"
        );
        assert_eq!(dept_label(Some(id), None, Some("旧车间".to_string())), "旧车间");
        assert_eq!(dept_label(Some(id), None, None), id.to_string());
        assert_eq!(dept_label(None, None, None), "未指定车间");
    }
}
